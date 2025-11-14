#!/usr/bin/env python3
"""Run targeted replay checks to detect search regressions.

This script reads scenarios from a TOML file, replays the specified prefixes
with `scripts/analysis/replay_multipv.sh`, and verifies that evaluations,
seldepth, and best moves stay within the expected bounds. It is intended for
manual regression sweeps (not CI) because it depends on deterministic timing
and available CPU threads.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional

try:  # Python 3.11+
    import tomllib  # type: ignore[attr-defined]
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore[no-redef]

RE_BESTMOVE = re.compile(r"^(pre-(?P<num>\d+)): bestmove=(?P<move>\S+)")
RE_INFO = re.compile(
    r"^(pre-(?P<num>\d+)): last_info=.*depth (?P<depth>\d+) seldepth (?P<seldepth>\d+).*score\s+(cp|mate)\s+(?P<score>-?\d+)"
)


def _run(cmd: List[str], cwd: Path) -> None:
    subprocess.run(cmd, cwd=cwd, check=True)


@dataclass
class PrefixGuard:
    number: int
    allowed_moves: Optional[List[str]] = None
    min_cp: Optional[int] = None
    max_cp: Optional[int] = None


@dataclass
class Scenario:
    name: str
    log: str
    prefixes: List[int]
    threads: int = 8
    multipv: int = 1
    byoyomi_ms: int = 10000
    engine: str = "target/release/engine-usi"
    out_dir: Optional[str] = None
    score_cp_min: Optional[int] = None
    score_cp_max: Optional[int] = None
    seldepth_max: Optional[int] = None
    prefix_guards: List[PrefixGuard] = field(default_factory=list)

    @staticmethod
    def from_dict(data: Dict) -> "Scenario":
        guards = [PrefixGuard(**pg) for pg in data.get("prefix_guard", [])]
        return Scenario(
            name=data["name"],
            log=data["log"],
            prefixes=list(data["prefixes"]),
            threads=data.get("threads", 8),
            multipv=data.get("multipv", 1),
            byoyomi_ms=data.get("byoyomi_ms", 10000),
            engine=data.get("engine", "target/release/engine-usi"),
            out_dir=data.get("out_dir"),
            score_cp_min=data.get("score_cp_min"),
            score_cp_max=data.get("score_cp_max"),
            seldepth_max=data.get("seldepth_max"),
            prefix_guards=guards,
        )


@dataclass
class PrefixResult:
    bestmove: str
    depth: int
    seldepth: int
    score_cp: int


class RegressionSuite:
    def __init__(self, repo_root: Path, config: Path, scenarios: List[str] | None):
        self.repo_root = repo_root
        with config.open("rb") as f:
            raw = tomllib.load(f)
        all_entries = raw.get("scenario", [])
        if not all_entries:
            raise SystemExit("Config file has no scenarios")
        self.scenarios = []
        for entry in all_entries:
            scn = Scenario.from_dict(entry)
            if scenarios and scn.name not in scenarios:
                continue
            self.scenarios.append(scn)
        if scenarios:
            missing = [name for name in scenarios if name not in {s.name for s in self.scenarios}]
            if missing:
                raise SystemExit(f"Unknown scenarios requested: {missing}")

    def run(self) -> int:
        failures = []
        for scn in self.scenarios:
            print(f"[regressions] scenario={scn.name}")
            try:
                summary = self._run_replay(scn)
                results = self._parse_summary(summary)
                self._check_bounds(scn, results)
                print(f"  -> PASS ({len(results)} prefixes)")
            except Exception as exc:  # pragma: no cover - CLI surface
                failures.append((scn.name, str(exc)))
                print(f"  -> FAIL: {exc}")
        if failures:
            print("\nFailures:")
            for name, msg in failures:
                print(f"  - {name}: {msg}")
            return 1
        return 0

    def _run_replay(self, scn: Scenario) -> Path:
        out_dir = Path(scn.out_dir or f"runs/regressions/{scn.name}")
        out_dir.mkdir(parents=True, exist_ok=True)
        prefix_arg = " ".join(str(p) for p in scn.prefixes)
        cmd = [
            "bash",
            "scripts/analysis/replay_multipv.sh",
            scn.log,
            "-p",
            prefix_arg,
            "-e",
            scn.engine,
            "-o",
            str(out_dir),
            "-m",
            str(scn.multipv),
            "-t",
            str(scn.threads),
            "-b",
            str(scn.byoyomi_ms),
        ]
        _run(cmd, self.repo_root)
        summary = out_dir / "summary.txt"
        if not summary.exists():
            raise RuntimeError(f"summary not found: {summary}")
        return summary

    def _parse_summary(self, summary_path: Path) -> Dict[int, PrefixResult]:
        results: Dict[int, PrefixResult] = {}
        with summary_path.open() as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                m = RE_BESTMOVE.match(line)
                if m:
                    num = int(m.group("num"))
                    best = m.group("move")
                    results.setdefault(num, PrefixResult(best, 0, 0, 0)).bestmove = best
                    continue
                m = RE_INFO.match(line)
                if m:
                    num = int(m.group("num"))
                    depth = int(m.group("depth"))
                    seldepth = int(m.group("seldepth"))
                    score = int(m.group("score"))
                    if line.find("score mate") != -1:
                        score = 100000 if score > 0 else -100000
                    res = results.setdefault(num, PrefixResult("(unknown)", 0, 0, 0))
                    res.depth = depth
                    res.seldepth = seldepth
                    res.score_cp = score
        missing = [p for p in results if results[p].bestmove == "(unknown)"]
        if missing:
            raise RuntimeError(f"summary missing data for prefixes: {missing}")
        return results

    def _check_bounds(self, scn: Scenario, results: Dict[int, PrefixResult]) -> None:
        for prefix in scn.prefixes:
            if prefix not in results:
                raise RuntimeError(f"prefix pre-{prefix} missing in summary")
            res = results[prefix]
            if scn.score_cp_min is not None and res.score_cp < scn.score_cp_min:
                raise RuntimeError(
                    f"pre-{prefix}: score {res.score_cp} < min {scn.score_cp_min}"
                )
            if scn.score_cp_max is not None and res.score_cp > scn.score_cp_max:
                raise RuntimeError(
                    f"pre-{prefix}: score {res.score_cp} > max {scn.score_cp_max}"
                )
            if scn.seldepth_max is not None and res.seldepth > scn.seldepth_max:
                raise RuntimeError(
                    f"pre-{prefix}: seldepth {res.seldepth} > max {scn.seldepth_max}"
                )
        guard_map = {pg.number: pg for pg in scn.prefix_guards}
        for number, guard in guard_map.items():
            res = results.get(number)
            if res is None:
                raise RuntimeError(f"prefix guard pre-{number} missing")
            if guard.allowed_moves and res.bestmove not in guard.allowed_moves:
                raise RuntimeError(
                    f"pre-{number}: bestmove {res.bestmove} not in {guard.allowed_moves}"
                )
            if guard.min_cp is not None and res.score_cp < guard.min_cp:
                raise RuntimeError(
                    f"pre-{number}: score {res.score_cp} < guard min {guard.min_cp}"
                )
            if guard.max_cp is not None and res.score_cp > guard.max_cp:
                raise RuntimeError(
                    f"pre-{number}: score {res.score_cp} > guard max {guard.max_cp}"
                )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--config",
        default="scripts/analysis/regression_scenarios.toml",
        help="Path to the regression scenario config",
    )
    parser.add_argument(
        "--scenario",
        action="append",
        help="Scenario name to run (repeatable). Default: run all",
    )
    args = parser.parse_args()
    repo_root = Path(__file__).resolve().parents[2]
    suite = RegressionSuite(repo_root, Path(args.config), args.scenario)
    return suite.run()


if __name__ == "__main__":
    sys.exit(main())
