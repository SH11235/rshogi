"""成果物の出力先を ``$SHOGI_DATA`` の決まった subtree に限る。"""

from __future__ import annotations

import os
from pathlib import Path

_ALLOWED_SUBDIRS = (Path("runs") / "rshogi-dl", Path("dl") / "transformer-v1-validation")


class ArtifactPathError(ValueError):
    """成果物を書けない出力先。"""


def shogi_data_root() -> Path:
    """``$SHOGI_DATA`` を返す。未設定や相対 path は拒否する。"""
    raw = os.environ.get("SHOGI_DATA")
    if not raw:
        raise ArtifactPathError("SHOGI_DATA is not set")
    root = Path(raw)
    if not root.is_absolute():
        raise ArtifactPathError("SHOGI_DATA must be an absolute path")
    return root.resolve()


def allowed_roots() -> tuple[Path, ...]:
    """成果物を置ける directory (``runs/rshogi-dl`` と ``dl/transformer-v1-validation``)。"""
    root = shogi_data_root()
    return tuple(root / subdir for subdir in _ALLOWED_SUBDIRS)


def _repository_roots() -> set[Path]:
    roots: set[Path] = set()
    for start in (Path(__file__).resolve(), Path.cwd().resolve()):
        for candidate in (start, *start.parents):
            # worktree では .git が file になる。
            if (candidate / ".git").exists():
                roots.add(candidate)
                break
    return roots


def resolve_new_artifact(path: str | Path) -> Path:
    """新規に作る成果物の path を検証して絶対 path で返す。

    許可された subtree の外、repository の中、既に存在する path を拒否する。
    """
    candidate = Path(path)
    if not candidate.is_absolute():
        raise ArtifactPathError(f"artifact path must be absolute: {candidate}")
    resolved = candidate.resolve()
    roots = allowed_roots()
    if not any(resolved != root and resolved.is_relative_to(root) for root in roots):
        allowed = ", ".join(f"$SHOGI_DATA/{subdir.as_posix()}" for subdir in _ALLOWED_SUBDIRS)
        raise ArtifactPathError(f"artifact path must be inside one of: {allowed}")
    for repository in _repository_roots():
        if resolved.is_relative_to(repository):
            raise ArtifactPathError("artifact path must not be inside a source repository")
    if os.path.lexists(candidate) or os.path.lexists(resolved):
        raise ArtifactPathError(f"refusing to overwrite an existing path: {resolved}")
    return resolved
