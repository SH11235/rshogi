#!/usr/bin/env python3
import argparse
import csv
import json
import os
import re


BESTMOVE_RE = re.compile(r"\bbestmove\s+([^\s]+)")


def read_targets(outdir):
    data = json.load(open(os.path.join(outdir, 'targets.json'), 'r', encoding='utf-8'))
    idx = {}
    for t in data.get('targets', []):
        idx[t['tag']] = t
    return idx


def ensure_first_bad_csv(outdir):
    fb = os.path.join(outdir, 'true_blunders_first_bad.csv')
    if os.path.exists(fb):
        return fb
    # try to generate via summarize_true_blunders.py
    import subprocess, sys
    try:
        subprocess.run([sys.executable, 'scripts/analysis/summarize_true_blunders.py', outdir], check=True)
    except Exception:
        pass
    return fb if os.path.exists(fb) else None


def derive_first_bad_rows_from_summary(outdir, profile, bad_th=-600):
    # Build origin/back from targets.json and choose first tag per origin with cp<=bad_th
    data = json.load(open(os.path.join(outdir, 'targets.json'), 'r', encoding='utf-8'))
    targets = data.get('targets', [])
    by_origin = {}
    for t in targets:
        origin = t.get('origin_log') or ''
        if not origin:
            continue
        by_origin.setdefault(origin, []).append({'tag': t['tag'], 'back': int(t.get('back_plies') or 0), 'origin': f"{origin}:{t.get('origin_ply','')}"})
    rows = json.load(open(os.path.join(outdir, 'summary.json'), 'r', encoding='utf-8'))
    cp_idx = {(r.get('tag'), r.get('profile')): r.get('eval_cp') for r in rows}
    out = []
    for origin, items in by_origin.items():
        items.sort(key=lambda x: x['back'])
        chosen = None
        for it in items:
            cp = cp_idx.get((it['tag'], profile))
            if cp is not None and cp <= bad_th:
                chosen = it
                break
        if chosen:
            out.append(chosen)
    return out


def load_first_bad_rows(outdir):
    path = ensure_first_bad_csv(outdir)
    rows = []
    if not path:
        return rows
    with open(path, 'r', encoding='utf-8') as f:
        rd = csv.DictReader(f)
        rows.extend(rd)
    return rows


def parse_bestmoves_with_positions(lines):
    best = []
    cur_eval = None
    last_cp = None
    last_depth = -1
    for i, l in enumerate(lines):
        if ' info ' in l and ' score ' in l:
            m = re.search(r"info\s+depth\s+(\d+).*?score cp\s+([+-]?\d+)", l)
            if m:
                last_depth = int(m.group(1)); last_cp = int(m.group(2)); cur_eval = last_cp
        if ' bestmove ' in l:
            # find next position line
            pos_after = None
            for j in range(i + 1, min(i + 80, len(lines))):
                if ' position ' in lines[j] and ' moves ' in lines[j]:
                    try:
                        pos_after = lines[j].split(' position ', 1)[1].strip()
                    except Exception:
                        pass
                    break
            m2 = re.search(r'bestmove\s+([\S]+)', l)
            bm = m2.group(1) if m2 else None
            best.append({
                'idx': i + 1,
                'bestmove': bm,
                'pos_after': pos_after,
                'last_cp': last_cp,
                'last_depth': last_depth,
            })
    return best


def original_bad_move_for_tag(tag_row, targets_idx):
    tag = tag_row['tag']
    t = targets_idx.get(tag)
    if not t:
        return None
    origin = tag_row.get('origin') or (t.get('origin_log', '') + ':' + str(t.get('origin_ply', '')))
    origin_file = origin.split(':', 1)[0]
    back = int(tag_row.get('back', t.get('back_plies', 0) or 0))
    origin_log_path = os.path.join('taikyoku-log', origin_file)
    try:
        lines = [ln.rstrip('\n') for ln in open(origin_log_path, 'r', encoding='utf-8', errors='ignore')]
    except FileNotFoundError:
        return None
    best = parse_bestmoves_with_positions(lines)
    try:
        origin_ply = int((t.get('origin_ply') or origin.split(':',1)[1]))
    except Exception:
        return None
    if origin_ply < 1 or origin_ply > len(best):
        return None
    pos_after = best[origin_ply - 1].get('pos_after') or ''
    if ' moves ' not in pos_after:
        return None
    head, moves_str = pos_after.split(' moves ', 1)
    toks = [tok for tok in moves_str.strip().split() if tok]
    if back <= 0 or len(toks) < back:
        return None
    # The next move after pre_position is the first of the chopped suffix
    bad_mv = toks[-back]
    return bad_mv


def evaluated_bestmove(outdir, tag, profile):
    log = os.path.join(outdir, f"{tag}__{profile}.log")
    if not os.path.exists(log):
        return None
    last = None
    for ln in open(log, 'r', encoding='utf-8', errors='ignore'):
        m = BESTMOVE_RE.search(ln)
        if m:
            last = m.group(1)
    return last


def load_eval_cp(summary_path, tag, profile):
    try:
        rows = json.load(open(summary_path, 'r', encoding='utf-8'))
    except Exception:
        return None
    for r in rows:
        if r.get('tag') == tag and r.get('profile') == profile:
            return r.get('eval_cp')
    return None


def main():
    ap = argparse.ArgumentParser(description='Compute bad-move avoidance rate on first_bad targets')
    ap.add_argument('outdir')
    ap.add_argument('--profile', required=True)
    ap.add_argument('--good-th', type=int, default=-200, help='threshold cp to treat as non-low after avoidance (default: -200)')
    args = ap.parse_args()

    targets_idx = read_targets(args.outdir)
    first_bad_rows = load_first_bad_rows(args.outdir)
    if not first_bad_rows:
        first_bad_rows = derive_first_bad_rows_from_summary(args.outdir, args.profile)
    if not first_bad_rows:
        print(json.dumps({'error': 'no_first_bad'}, ensure_ascii=False))
        return
    summary_path = os.path.join(args.outdir, 'summary.json')
    out_csv = os.path.join(args.outdir, f'avoidance_{args.profile}.csv')
    total = 0
    avoided = 0
    avoided_good = 0
    rows_out = []
    for row in first_bad_rows:
        tag = row['tag']
        total += 1
        orig_bad = original_bad_move_for_tag(row, targets_idx)
        eval_best = evaluated_bestmove(args.outdir, tag, args.profile)
        eval_cp = load_eval_cp(summary_path, tag, args.profile)
        av = (orig_bad is not None and eval_best is not None and eval_best != orig_bad)
        good = (eval_cp is not None and eval_cp >= args.good_th)
        if av:
            avoided += 1
            if good:
                avoided_good += 1
        rows_out.append({
            'tag': tag,
            'origin': row.get('origin', ''),
            'back': row.get('back', ''),
            'bad_move': orig_bad or '',
            'eval_profile': args.profile,
            'eval_bestmove': eval_best or '',
            'eval_cp': eval_cp if eval_cp is not None else '',
            'avoided': '1' if av else '0',
            'avoided_and_good': '1' if (av and good) else '0',
        })

    with open(out_csv, 'w', encoding='utf-8', newline='') as wf:
        w = csv.DictWriter(wf, fieldnames=list(rows_out[0].keys()))
        w.writeheader()
        w.writerows(rows_out)

    out_json = os.path.join(args.outdir, f'avoidance_{args.profile}.json')
    res = {
        'profile': args.profile,
        'first_bad_total': total,
        'avoid_count': avoided,
        'avoid_rate_percent': (avoided / total * 100.0) if total else None,
        'avoid_and_good_count': avoided_good,
        'avoid_and_good_rate_percent': (avoided_good / total * 100.0) if total else None,
        'good_threshold_cp': args.good_th,
        'csv': out_csv,
    }
    with open(out_json, 'w', encoding='utf-8') as wf:
        json.dump(res, wf, ensure_ascii=False, indent=2)
    print(json.dumps(res, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
