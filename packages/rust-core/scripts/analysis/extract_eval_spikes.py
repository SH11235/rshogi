#!/usr/bin/env python3
import argparse
import os
import re
import sys
from typing import List, Tuple


INFO_RE = re.compile(r"\binfo\b.*?score\s+(cp|mate)\s+(-?\d+)")
BESTMOVE_RE = re.compile(r"\bbestmove\s+([^\s]+)")
POS_MOVES_RE = re.compile(r"position\s+startpos\s+moves\s+(.+)$")


def parse_log(path: str, include_re: re.Pattern | None, exclude_re: re.Pattern | None) -> Tuple[List[int], List[str]]:
    """Parse a USI log and return per-ply evals (cp, side-to-move perspective)
    and the list of bestmove USI strings in order.

    We take the last seen `info ... score` before each `bestmove` as the
    evaluation for that ply. If no score was seen, we mark as None and skip
    delta computation for that ply.
    """
    evals: List[int] = []
    moves: List[str] = []
    cur_eval: int | None = None

    with open(path, "r", encoding="utf-8", errors="ignore") as f:
        for line in f:
            if include_re and not include_re.search(line):
                continue
            if exclude_re and exclude_re.search(line):
                continue
            m = INFO_RE.search(line)
            if m:
                kind, val = m.group(1), int(m.group(2))
                if kind == "cp":
                    cur_eval = val
                else:
                    # Treat mate distances as large centipawn values for spike detection
                    # Positive = giving mate; Negative = mated
                    cur_eval = 100000 if val > 0 else -100000
                continue
            m = BESTMOVE_RE.search(line)
            if m:
                mv = m.group(1)
                moves.append(mv)
                # If we never saw a score before this bestmove, carry forward last known eval
                if cur_eval is None and evals:
                    evals.append(evals[-1])
                elif cur_eval is None:
                    evals.append(0)
                else:
                    evals.append(cur_eval)
                cur_eval = None
                continue

    return evals, moves


def compute_spikes(evals: List[int], threshold: int) -> List[Tuple[int, int]]:
    """Return list of (ply_index, delta) where |delta| >= threshold.

    ply_index is 1-based count of bestmoves observed so far.
    """
    spikes: List[Tuple[int, int]] = []
    prev = None
    for i, sc in enumerate(evals, start=1):
        if prev is None:
            prev = sc
            continue
        delta = sc - prev
        if abs(delta) >= threshold:
            spikes.append((i, delta))
        prev = sc
    return spikes


def expand_windows(indices: List[int], back: int, forward: int, nmax: int) -> List[int]:
    wanted = set()
    for idx in indices:
        a = max(1, idx - back)
        b = min(nmax, idx + forward)
        for k in range(a, b + 1):
            wanted.add(k)
    return sorted(wanted)


def main():
    ap = argparse.ArgumentParser(description="Extract large eval spikes from a USI log and propose replay prefixes with context.")
    ap.add_argument("log", help="path to USI log (engine vs external engine)")
    ap.add_argument("--threshold", type=int, default=300, help="abs(cp) delta threshold to mark a spike (default: 300)")
    ap.add_argument("--back", type=int, default=3, help="prefix context: include N plies before each spike (default: 3)")
    ap.add_argument("--forward", type=int, default=2, help="prefix context: include N plies after each spike (default: 2)")
    ap.add_argument("--topk", type=int, default=0, help="if >0, only keep top-K spikes by abs(delta)")
    ap.add_argument("--out", default=None, help="output directory to write results (default: runs/analysis/spikes-<basename>)")
    ap.add_argument("--include", default=None, help="only process lines matching this regex (optional)")
    ap.add_argument("--exclude", default=None, help="skip lines matching this regex (optional)")
    args = ap.parse_args()

    include_re = re.compile(args.include) if args.include else None
    exclude_re = re.compile(args.exclude) if args.exclude else None
    evals, moves = parse_log(args.log, include_re, exclude_re)
    if not moves:
        print("No bestmove entries found; is this a USI log?", file=sys.stderr)
        sys.exit(1)

    spikes = compute_spikes(evals, args.threshold)
    if args.topk and len(spikes) > args.topk:
        spikes = sorted(spikes, key=lambda x: abs(x[1]), reverse=True)[: args.topk]

    base = os.path.basename(args.log)
    out_dir = args.out or os.path.join("runs", "analysis", f"spikes-{os.path.splitext(base)[0]}")
    os.makedirs(out_dir, exist_ok=True)

    # Write CSV of per-ply evals and deltas
    csv_path = os.path.join(out_dir, "evals.csv")
    with open(csv_path, "w", encoding="utf-8") as f:
        f.write("ply,move,eval_cp,delta_cp\n")
        prev = None
        for i, (mv, sc) in enumerate(zip(moves, evals), start=1):
            if prev is None:
                delta = 0
            else:
                delta = sc - prev
            f.write(f"{i},{mv},{sc},{delta}\n")
            prev = sc

    # Write spikes list
    spikes_path = os.path.join(out_dir, "spikes.csv")
    with open(spikes_path, "w", encoding="utf-8") as f:
        f.write("ply,delta_cp\n")
        for i, d in spikes:
            f.write(f"{i},{d}\n")

    # Propose prefix numbers for replay_multipv.sh (pre-N) with context window
    spike_indices = [i for i, _ in spikes]
    pre_list = expand_windows(spike_indices, args.back, args.forward, len(moves))
    # Convert to space-separated string
    prefixes = " ".join(str(p) for p in pre_list)
    with open(os.path.join(out_dir, "prefixes.txt"), "w", encoding="utf-8") as f:
        f.write(prefixes + "\n")

    # Human-friendly summary
    summary = os.path.join(out_dir, "summary.txt")
    with open(summary, "w", encoding="utf-8") as f:
        f.write(f"log={args.log}\n")
        f.write(f"threshold={args.threshold} back={args.back} forward={args.forward} topk={args.topk}\n")
        if args.include:
            f.write(f"include={args.include}\n")
        if args.exclude:
            f.write(f"exclude={args.exclude}\n")
        f.write(f"plies={len(moves)} spikes={len(spikes)}\n")
        f.write(f"prefixes: {prefixes}\n")

    print(f"Wrote: {summary}")
    print(f"       {csv_path}")
    print(f"       {spikes_path}")
    print(f"       prefixes for replay_multipv: {prefixes}")


if __name__ == "__main__":
    main()
