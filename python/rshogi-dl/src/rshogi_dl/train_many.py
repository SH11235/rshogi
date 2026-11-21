"""Run trainer argument lists in one process."""

import argparse
import json
from pathlib import Path
import sys

from .trainer import main as train


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="JSONの引数リストを使い、複数の学習を同じプロセスで順番に実行する。")
    parser.add_argument("jobs", type=Path, help="trainer の引数配列を並べた JSON ファイル")
    args = parser.parse_args(argv)
    try:
        jobs = json.loads(args.jobs.read_text(encoding="utf-8"))
        if not isinstance(jobs, list) or not jobs:
            raise ValueError("jobs must be a non-empty list")
        for index, arguments in enumerate(jobs, 1):
            if not isinstance(arguments, list) or not arguments or not all(isinstance(arg, str) for arg in arguments):
                raise ValueError(f"job {index} must be a non-empty list of argument strings")
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    for index, arguments in enumerate(jobs, 1):
        try:
            result = train(arguments)
        except SystemExit:
            result = 2
        if result:
            print(f"error: training stopped at job {index} of {len(jobs)}", file=sys.stderr)
            return result
        sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
