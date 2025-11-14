#!/usr/bin/env python3
import json
import argparse

def chop_moves(pos_line: str, back_plies: int) -> str:
    if not pos_line:
        return pos_line
    if ' moves ' not in pos_line:
        return pos_line
    head, moves = pos_line.split(' moves ', 1)
    toks = [t for t in moves.strip().split() if t]
    if back_plies > 0 and len(toks) >= back_plies:
        toks = toks[:-back_plies]
    return f"{head} moves {' '.join(toks)}" if toks else head

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--in', dest='in_path', required=True)
    ap.add_argument('--out', dest='out_path', required=True)
    ap.add_argument('--min', type=int, default=2)
    ap.add_argument('--max', type=int, default=5)
    args = ap.parse_args()

    data = json.load(open(args.in_path, 'r', encoding='utf-8'))
    base = data.get('targets', [])
    out = []
    for t in base:
        pos = t.get('pre_position') or t.get('pos_after')
        for k in range(args.min, args.max + 1):
            out.append({
                'tag': f"{t['tag']}_back{k}",
                'pre_position': chop_moves(pos, k),
                'origin': t['tag'],
                'back_plies': k,
            })
    json.dump({'targets': out}, open(args.out_path, 'w', encoding='utf-8'), ensure_ascii=False, indent=2)
    print(f"wrote {len(out)} targets to {args.out_path}")

if __name__ == '__main__':
    main()

