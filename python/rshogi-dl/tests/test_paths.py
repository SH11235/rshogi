from __future__ import annotations

import os
import unittest
from pathlib import Path
from unittest import mock

from rshogi_dl import paths
from rshogi_dl.paths import ArtifactPathError, resolve_new_artifact

from .support import artifact_tempdir


class ArtifactPathTest(unittest.TestCase):
    def test_accepts_new_paths_in_both_allowed_roots(self) -> None:
        for root_index in (0, 1):
            with artifact_tempdir(root_index) as directory:
                target = Path(directory) / "run" / "model.onnx"
                self.assertEqual(resolve_new_artifact(target), target.resolve())

    def test_allowed_roots_layout(self) -> None:
        data = paths.shogi_data_root()
        self.assertEqual(
            paths.allowed_roots(),
            (data / "runs" / "rshogi-dl", data / "dl" / "transformer-v1-validation"),
        )

    def test_rejects_existing_path(self) -> None:
        with artifact_tempdir() as directory:
            existing = Path(directory) / "checkpoint.pt"
            existing.write_bytes(b"")
            with self.assertRaisesRegex(ArtifactPathError, "overwrite"):
                resolve_new_artifact(existing)
            with self.assertRaisesRegex(ArtifactPathError, "overwrite"):
                resolve_new_artifact(directory)

    def test_rejects_paths_outside_allowed_roots(self) -> None:
        data = paths.shogi_data_root()
        root = paths.allowed_roots()[0]
        rejected = (
            root,
            data / "runs" / "other-trainer" / "run",
            data / "nnue" / "model.onnx",
            data / "dl" / "model.onnx",
            root / ".." / "escaped",
            Path("relative") / "run",
        )
        for target in rejected:
            with self.subTest(target=target), self.assertRaises(ArtifactPathError):
                resolve_new_artifact(target)

    def test_rejects_symlink_leaving_allowed_roots(self) -> None:
        with artifact_tempdir() as directory:
            link = Path(directory) / "link"
            link.symlink_to(paths.shogi_data_root())
            with self.assertRaises(ArtifactPathError):
                resolve_new_artifact(link / "nnue" / "model.onnx")

    def test_requires_shogi_data(self) -> None:
        environment = {key: value for key, value in os.environ.items() if key != "SHOGI_DATA"}
        with mock.patch.dict(os.environ, environment, clear=True):
            with self.assertRaisesRegex(ArtifactPathError, "SHOGI_DATA"):
                resolve_new_artifact("/data/runs/rshogi-dl/run")
        with mock.patch.dict(os.environ, {"SHOGI_DATA": "relative/data"}):
            with self.assertRaisesRegex(ArtifactPathError, "absolute"):
                resolve_new_artifact("/data/runs/rshogi-dl/run")

    def test_rejects_paths_inside_the_repository(self) -> None:
        package_root = Path(__file__).resolve().parents[1]
        if not any((parent / ".git").exists() for parent in package_root.parents):
            self.skipTest("the package is not inside a source checkout")
        # 何も作らずに判定だけを確かめる。
        inside = package_root / "shogi-data-inside-repo"
        with mock.patch.dict(os.environ, {"SHOGI_DATA": str(inside)}):
            with self.assertRaisesRegex(ArtifactPathError, "repository"):
                resolve_new_artifact(inside / "runs" / "rshogi-dl" / "run")


if __name__ == "__main__":
    unittest.main()
