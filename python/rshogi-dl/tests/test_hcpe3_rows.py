import io
from pathlib import Path
import struct
import unittest
from unittest import mock

from rshogi_dl.hcpe3_rows import GameFrame, Hcpe3RowsError, PolicyFrame, iter_frames
from rshogi_dl.sharded import DatasetError, convert_hcpe3, convert_records, verify_dataset

from .support import artifact_tempdir, empty_board_tokens


def source_stream() -> bytes:
    return (
        b"RSDLHC01" + struct.pack("<32sHH", b"a" * 32, 0, 0)
        + struct.pack("<32sHH", b"b" * 32, 2, 1)
        + struct.pack("<H", 1) + bytes(empty_board_tokens())
        + struct.pack("<HHHB", 13, 2, 1, 2) + struct.pack("<HHHH", 13, 46, 46, 19)
    )


class ShortReader(io.BytesIO):
    def read(self, size=-1):
        return super().read(min(3, size) if size >= 0 else size)


class Hcpe3RowsTest(unittest.TestCase):
    def test_empty_game_and_positive_visit_row_survive_short_reads(self):
        first, second, row = list(iter_frames(ShortReader(source_stream())))
        self.assertEqual(first, GameFrame(b"a" * 32, 0, 0))
        self.assertEqual(second, GameFrame(b"b" * 32, 2, 1))
        self.assertIsInstance(row, PolicyFrame)
        self.assertEqual((row.ply, row.played), (1, 13))
        self.assertEqual(row.to_record(), {
            "tokens": list(empty_board_tokens()), "legal_labels": [13, 46], "policy": [[46, 19]], "value": 1.0,
        })

    def test_rejects_truncated_last_record(self):
        with self.assertRaisesRegex(Hcpe3RowsError, "truncated"):
            list(iter_frames(io.BytesIO(source_stream()[:-1])))

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

                    def finish():
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
