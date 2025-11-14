#!/usr/bin/env python3
import re
import json
import argparse


def parse_bestmoves(lines):
    best = []
    pos_after = {}
    # index positions where a position line occurs
    for i, l in enumerate(lines):
        if ' position startpos moves ' in l:
            pos_after.setdefault(i + 1, l.split(' position ', 1)[1].strip())
    for i, l in enumerate(lines):
        if ' bestmove ' in l:
            # find the next position line after bestmove (GUI sends after engine reply)
            pos_line = None
            for j in range(i + 1, min(i + 30, len(lines))):
                if ' position startpos moves ' in lines[j]:
                    pos_line = lines[j].split(' position ', 1)[1].strip()
                    break
            m = re.search(r'bestmove\s+([\S]+)', l)
            bm = m.group(1) if m else None
            # Also capture last cp/depth before this bestmove
            cp, depth = None, None
            for k in range(i, max(i - 120, -1), -1):
                m2 = re.search(r'info\s+depth\s+(\d+).*?score cp\s+([+-]?\d+)', lines[k])
                if m2:
                    depth = int(m2.group(1)); cp = int(m2.group(2)); break
            best.append({
                'idx': i + 1,
                'bestmove': bm,
                'pos_after': pos_line,
                'last_cp': cp,
                'last_depth': depth,
            })
    return best


def strip_last_two_moves(pos_line):
    # pos_line: "startpos moves <m1 m2 ...>" or "sfen ... moves <...>"
    if not pos_line:
        return None
    try:
        if pos_line.startswith('startpos'):
            head, moves = pos_line.split(' moves ', 1)
            toks = moves.strip().split()
            if len(toks) >= 2:
                toks = toks[:-2]
            return f"startpos moves {' '.join(toks)}" if toks else 'startpos'
        else:
            head, moves = pos_line.split(' moves ', 1)
            toks = moves.strip().split()
            if len(toks) >= 2:
                toks = toks[:-2]
            return f"{head} moves {' '.join(toks)}" if toks else head
    except ValueError:
        return pos_line


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--log', required=True)
    ap.add_argument('--targets', default='')
    ap.add_argument('--out', required=True)
    args = ap.parse_args()
    with open(args.log, encoding='utf-8', errors='ignore') as f:
        lines = [ln.rstrip('\n') for ln in f]
    best = parse_bestmoves(lines)

    # known largest-drop indices from prior scan (0-based in our list)
    # We defensively clamp to available range
    drop_indices = sorted(set([21, 28, 32, 33, 35]))  # from earlier analysis windows
    targets = []
    for di in drop_indices:
        if di < 0 or di >= len(best):
            continue
        b = best[di]
        pre = strip_last_two_moves(b['pos_after'])  # position before our bestmove
        # post = b['pos_after']  # optional
        targets.append({
            'tag': f'drop_{di:02d}_line{b["idx"]}',
            'bestmove': b['bestmove'],
            'pre_position': pre,
            'last_cp': b['last_cp'],
            'last_depth': b['last_depth'],
        })
    with open(args.out, 'w', encoding='utf-8') as wf:
        json.dump({'targets': targets}, wf, ensure_ascii=False, indent=2)
    print(f"wrote {len(targets)} targets -> {args.out}")


if __name__ == '__main__':
    main()

