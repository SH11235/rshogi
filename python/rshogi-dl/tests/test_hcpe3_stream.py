from hashlib import sha256
import json
import lzma
import os
from pathlib import Path
import shutil
import struct
import unittest
from unittest import mock

import torch

from rshogi_dl.hcpe3_stream import FORMAT, hcpe3_dataset_info, iter_hcpe3_batches

from .support import artifact_tempdir, empty_board_tokens


def _archive(policy_label, game_keys):
    frames = [b"RSDLHC01"]
    tokens = bytes(empty_board_tokens())
    for index, key in enumerate(game_keys):
        label = policy_label + index
        frames.extend((
            struct.pack("<32sHHH", bytes([key]) * 32, 1, 1, 0), tokens,
            struct.pack("<HHHBHHH", label, 1, 1, 2, label, label, 5),
        ))
    return b"".join(frames)


def _identity(path):
    return {"path": str(path), "sha256": sha256(path.read_bytes()).hexdigest()}


@unittest.skipUnless(os.name == "posix" and shutil.which("xz"), "requires an executable fixture and xz")
class Hcpe3StreamTest(unittest.TestCase):
    def test_interleave_protection_and_group_resume(self):
        with artifact_tempdir() as temporary, mock.patch("rshogi_dl.hcpe3_stream._SHUFFLE_ROWS", 4):
            root = Path(temporary)
            encoder = root / "encoder"
            encoder.write_text(
                "#!/usr/bin/env python3\n"
                "from pathlib import Path\n"
                "import sys\n"
                "content = sys.stdin.buffer.read() if sys.argv[-1] == '-' else Path(sys.argv[-1]).read_bytes()\n"
                "sys.stdout.buffer.write(content)\n"
            )
            encoder.chmod(0o755)
            first, second, third = (root / name for name in ("first.hcpe3", "second.hcpe3.xz", "third.hcpe3"))
            first.write_bytes(_archive(13, range(1, 7)))
            second.write_bytes(lzma.compress(_archive(46, (1, 10, 11, 12))))
            third.write_bytes(_archive(57, (20, 21, 22)))
            protected = root / "protected.bin"
            protected.write_bytes(bytes([1]) * 32)
            manifest = root / "input.json"
            config = {
                "format": FORMAT, "encoder": str(encoder), "encoder_sha256": _identity(encoder)["sha256"],
                "archives": [dict(_identity(path), size=path.stat().st_size) for path in (first, second, third)],
                "protected_tokens": None, "protected_games": _identity(protected), "heldout_modulus": 0,
                "seed": 42, "interleave_archives": 2,
            }
            manifest.write_text(json.dumps(config))
            fingerprint = hcpe3_dataset_info(manifest)["fingerprint"]

            def labels(start):
                batches = iter_hcpe3_batches(manifest, 4, start_row=start, expected_fingerprint=fingerprint)
                return torch.cat([batch.legal_index[:, 0] for batch in batches]).tolist()

            partial = labels(4)
            full = labels(0)
            self.assertEqual(partial, full[4:])
            self.assertCountEqual(full[:4], [14, 15, 47, 48])
            self.assertCountEqual(full[:8], [14, 15, 16, 17, 18, 47, 48, 49])
            self.assertCountEqual(full[8:], [57, 58, 59])
            self.assertEqual(labels(4), full[4:])
            self.assertEqual(labels(8), full[8:])
