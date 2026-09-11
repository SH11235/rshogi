#!/usr/bin/env python3
"""公開 PV の変更前後の release テストバイナリを ABBA 順に測定する。"""
import argparse
import csv
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("fixed", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--cpus", default="", help="taskset の CPU リスト。1T は先頭だけを使用")
    args = parser.parse_args()
    binaries = {name: str(getattr(args, name).resolve()) for name in ("baseline", "fixed")}
    if str(args.output.resolve()) in binaries.values():
        parser.error("output must differ from the benchmark binaries")
    fields = ["variant", "block", "order", "mode", "threads", "position", "multipv",
              "ns", "nodes", "depth", "score", "bestmove", "callbacks", "emitted_moves"]
    with args.output.open("w", newline="") as output:
        writer = csv.writer(output, lineterminator="\n")
        writer.writerow(fields)
        for mode, threads, blocks in [("nodes", 1, 8), ("depth", 1, 4),
                                      ("time", 1, 8), ("time", 2, 8)]:
            env = dict(os.environ, PV_BENCH_MODE=mode, PV_BENCH_THREADS=str(threads))
            for block in range(blocks):
                for order, variant in enumerate(["baseline", "fixed", "fixed", "baseline"]):
                    command = [binaries[variant], "public_pv_bench", "--ignored", "--nocapture"]
                    if args.cpus:
                        cpus = args.cpus.split(",")[0] if threads == 1 else args.cpus
                        command = ["taskset", "-c", cpus] + command
                    result = subprocess.run(command, env=env, capture_output=True, text=True)
                    if result.returncode:
                        sys.stderr.write(result.stdout + result.stderr)
                        raise SystemExit(result.returncode)
                    rows = [line.split("PV_BENCH,", 1)[1].split(",")
                            for line in result.stdout.splitlines() if "PV_BENCH," in line]
                    if len(rows) != 6:
                        raise RuntimeError(f"expected 6 benchmark records, got {len(rows)}")
                    for row in rows:
                        writer.writerow([variant, block, order] + row)
                    output.flush()
                print(f"{mode} threads={threads} block={block + 1}/{blocks}", file=sys.stderr)


if __name__ == "__main__":
    main()
