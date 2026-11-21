import io
import os
from pathlib import Path
import signal
import struct
import subprocess
import unittest
from unittest import mock

from rshogi_dl.hcpe3_rows import (
    GameFrame, Hcpe3RowsError, PackedPolicyFrame, PolicyFrame, assemble_shards, iter_frames, iter_packed_frames,
)
from rshogi_dl.sharded import DatasetError, convert_hcpe3, convert_records, verify_dataset

from .support import artifact_tempdir, empty_board_tokens


def source_stream(*, played=13, visits=(46, 19)) -> bytes:
    return (
        b"RSDLHC01" + struct.pack("<32sHH", b"a" * 32, 0, 0)
        + struct.pack("<32sHH", b"b" * 32, 2, 1)
        + struct.pack("<H", 1) + bytes(empty_board_tokens())
        + struct.pack("<HHHB", played, 2, len(visits) // 2, 2)
        + struct.pack(f"<{2 + len(visits)}H", 13, 46, *visits)
    )


def policy_frame(tokens, legal, visits):
    payload = struct.pack(f"<{len(legal) + len(visits)}H", *legal, *visits)
    return PackedPolicyFrame(0, 1, tokens, 0.5, len(legal), payload)


class ShortReader(io.BytesIO):
    def read(self, size=-1):
        return super().read(min(3, size) if size >= 0 else size)


class Hcpe3RowsTest(unittest.TestCase):
    def test_shard_offsets_cross_assembly_blocks(self):
        row = policy_frame(bytes(empty_board_tokens()), (3, 1, 5), (5, 2, 3, 1))
        (columns, count), = assemble_shards(iter([row] * 4097), 5000)
        self.assertEqual(count, 4097)
        self.assertEqual(columns["legal_offsets"][-3:].tolist(), [12285, 12288, 12291])
        self.assertEqual(columns["legal_indices"][-3:].tolist(), [3, 1, 5])
        for got, want in zip(columns["policy_targets"][-3:], (1 / 3, 0.0, 2 / 3), strict=True):
            self.assertAlmostEqual(float(got), want)

    def test_shard_assembly_rejects_invalid_tokens_and_labels(self):
        tokens = bytes(empty_board_tokens())
        for row, message in (
            (policy_frame(b"\xff" + tokens[1:], (1, 2), (1, 1)), "tokens"),
            (policy_frame(tokens, (1, 2), (3, 1)), "not legal"),
            (policy_frame(tokens, (1, 1), (1, 1)), "duplicates"),
            (policy_frame(tokens, (1, 2), (1, 1, 1, 2)), "duplicates"),
        ):
            with self.subTest(message=message), self.assertRaisesRegex(Hcpe3RowsError, message):
                list(assemble_shards(iter([row]), 4))

    def test_empty_game_and_positive_visit_row_survive_short_reads(self):
        first, second, row = list(iter_frames(ShortReader(source_stream())))
        self.assertEqual(first, GameFrame(b"a" * 32, 0, 0))
        self.assertEqual(second, GameFrame(b"b" * 32, 2, 1))
        self.assertIsInstance(row, PolicyFrame)
        self.assertEqual((row.ply, row.played), (1, 13))
        self.assertEqual(row, PolicyFrame(1, 13, bytes(empty_board_tokens()), 1.0, (13, 46), (46, 19)))
        self.assertEqual(row.to_record(), {
            "tokens": list(empty_board_tokens()), "legal_labels": [13, 46], "policy": [[46, 19]], "value": 1.0,
        })

    def test_rejects_truncated_last_record(self):
        with self.assertRaisesRegex(Hcpe3RowsError, "truncated"):
            list(iter_frames(io.BytesIO(source_stream()[:-1])))

    def test_many_source_names_fit_the_manifest(self):
        with artifact_tempdir() as temporary:
            root = Path(temporary)
            sources = [root / f"games-{index:04}.hcpe3" for index in range(20)]
            for source in sources:
                source.write_bytes(b"game")
            with mock.patch("rshogi_dl.sharded.subprocess.Popen") as popen:
                child = popen.return_value.__enter__.return_value
                child.stdout = io.BytesIO(source_stream())
                child.wait.return_value = child.poll.return_value = 0
                result = convert_hcpe3(sources, root / "dataset", encoder="tool")
                popen.assert_called_once_with(["tool", *sources], stdout=subprocess.PIPE)
            self.assertEqual(result["source_name"], "HCPE3 (20 files)")
            self.assertEqual(result["source_size"], 80)
            if os.name == "posix":
                unnamed = root / " "
                unnamed.write_bytes(b"game")
                with mock.patch("rshogi_dl.sharded.subprocess.Popen") as popen:
                    child = popen.return_value.__enter__.return_value
                    child.stdout = io.BytesIO(source_stream())
                    child.wait.return_value = child.poll.return_value = 0
                    result = convert_hcpe3([unnamed], root / "unnamed", encoder="tool")
                self.assertEqual(verify_dataset(root / "unnamed"), result)

    def test_rejects_illegal_played_labels_and_zero_visits(self):
        for arguments, message in (
            ({"played": 22}, "played label is not legal"),
            ({"visits": (13, 0, 46, 19)}, "visit counts must be positive"),
        ):
            with self.subTest(arguments=arguments), self.assertRaisesRegex(Hcpe3RowsError, message):
                list(iter_frames(io.BytesIO(source_stream(**arguments))))
            rows = (frame for frame in iter_packed_frames(ShortReader(source_stream(**arguments)))
                    if isinstance(frame, PackedPolicyFrame))
            with self.subTest(arguments=arguments), self.assertRaisesRegex(Hcpe3RowsError, message):
                list(assemble_shards(rows, 4))

    def test_conversion_checks_encoder_and_sources_before_manifest_publication(self):
        with artifact_tempdir() as temporary:
            root = Path(temporary)
            source = root / "game.hcpe3"
            source.write_bytes(b"immutable")
            row = list(iter_frames(io.BytesIO(source_stream())))[-1]
            reference = convert_records(
                iter([row.to_record()]), root / "reference", source_name=source.name,
                source_size=source.stat().st_size, shard_records=1,
            )
            for purpose in ("valid", "encoder-error", "source-change"):
                with self.subTest(purpose=purpose), mock.patch("rshogi_dl.sharded.subprocess.Popen") as popen:
                    child = popen.return_value.__enter__.return_value
                    child.stdout = io.BytesIO(source_stream())

                    def finish(*, timeout):
                        if purpose == "source-change":
                            source.write_bytes(b"updated")
                        return int(purpose == "encoder-error")

                    child.wait.side_effect = finish
                    child.poll.return_value = int(purpose == "encoder-error")
                    output = root / purpose
                    if purpose == "valid":
                        self.assertEqual(convert_hcpe3([source], output, encoder="tool", shard_records=1), reference)
                        self.assertEqual(verify_dataset(output), reference)
                    else:
                        with self.assertRaisesRegex(DatasetError, "encoder exited|source changed"):
                            convert_hcpe3([source], output, encoder="tool", shard_records=1)
                        self.assertFalse((output / "manifest.json").exists())

    @unittest.skipUnless(os.name == "posix", "requires POSIX process signals")
    def test_failed_conversion_reaps_an_encoder_that_ignores_termination(self):
        with artifact_tempdir() as temporary:
            root = Path(temporary)
            source = root / "game.hcpe3"
            source.write_bytes(b"immutable")
            encoder = root / "encoder"
            encoder.write_text(
                "#!/usr/bin/env python3\nimport signal, sys, time\n"
                "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                "sys.stdout.buffer.write(b'INVALID!')\nsys.stdout.buffer.flush()\n"
                "time.sleep(2)\n",
            )
            encoder.chmod(0o700)
            original_popen = subprocess.Popen
            launched = []

            def launch(*args, **kwargs):
                child = original_popen(*args, **kwargs)
                launched.append(child)
                return child

            output = root / "dataset"
            with (
                mock.patch("rshogi_dl.sharded._ENCODER_EXIT_GRACE_SECONDS", 0.05),
                mock.patch("rshogi_dl.sharded.subprocess.Popen", side_effect=launch),
                self.assertRaisesRegex(DatasetError, "unsupported HCPE3"),
            ):
                convert_hcpe3([source], output, encoder=encoder, shard_records=1)
            self.assertEqual(len(launched), 1)
            self.assertEqual(launched[0].returncode, -signal.SIGKILL)
            self.assertFalse((output / "manifest.json").exists())
