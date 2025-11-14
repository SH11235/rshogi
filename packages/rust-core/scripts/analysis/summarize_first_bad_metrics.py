#!/usr/bin/env python3
import argparse
import csv
import json
import os


def load_summary(outdir, profile=None):
    rows = json.load(open(os.path.join(outdir, 'summary.json'), 'r', encoding='utf-8'))
    if profile:
        rows = [r for r in rows if r.get('profile') == profile]
    return rows


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


def derive_first_bad_tags_from_summary(outdir, profile, bad_th=-600):
    # Build origin -> list[(back, tag, cp)] using targets.json and summary.json
    targets = json.load(open(os.path.join(outdir, 'targets.json'), 'r', encoding='utf-8')).get('targets', [])
    t_by_origin = {}
    for t in targets:
        origin = t.get('origin_log') or ''
        back = t.get('back_plies') or 0
        tag = t.get('tag')
        if origin and tag is not None:
            t_by_origin.setdefault(origin, []).append((int(back), tag))
    # index summary cp by (tag, profile)
    rows = json.load(open(os.path.join(outdir, 'summary.json'), 'r', encoding='utf-8'))
    cp_idx = {}
    for r in rows:
        cp_idx[(r.get('tag'), r.get('profile'))] = r.get('eval_cp')
    tags = []
    for origin, items in t_by_origin.items():
        items.sort(key=lambda x: x[0])
        chosen = None
        for back, tag in items:
            cp = cp_idx.get((tag, profile))
            if cp is not None and cp <= bad_th:
                chosen = tag
                break
        if chosen:
            tags.append(chosen)
    return tags


def load_first_bad_tags(outdir):
    fb = ensure_first_bad_csv(outdir)
    tags = []
    if fb and os.path.exists(fb):
        with open(fb, 'r', encoding='utf-8') as f:
            rd = csv.DictReader(f)
            for row in rd:
                tag = row.get('tag')
                if tag:
                    tags.append(tag)
    return tags


def compute_metrics(rows):
    total = len(rows)
    valid = 0
    bad = 0
    cp_sum = 0
    depth_sum = 0
    for r in rows:
        cp = r.get('eval_cp')
        d = r.get('depth', -1)
        if cp is not None:
            valid += 1
            cp_sum += cp
            if cp <= -600:
                bad += 1
        if isinstance(d, int) and d >= 0:
            depth_sum += d
    avg_cp = (cp_sum / valid) if valid else None
    avg_depth = (depth_sum / valid) if valid else None
    spike_rate = (bad / valid * 100.0) if valid else None
    return {
        'total': total,
        'valid': valid,
        'bad_th': -600,
        'bad_count': bad,
        'spike_rate_percent': spike_rate,
        'avg_cp': avg_cp,
        'avg_depth': avg_depth,
    }


def main():
    ap = argparse.ArgumentParser(description='Compute metrics restricted to first_bad tags only')
    ap.add_argument('outdir')
    ap.add_argument('--profile', default=None)
    args = ap.parse_args()

    rows = load_summary(args.outdir, args.profile)
    tags = set(load_first_bad_tags(args.outdir))
    if not tags and args.profile:
        # derive for the given profile
        tags = set(derive_first_bad_tags_from_summary(args.outdir, args.profile))
    if not tags:
        print(json.dumps({'error': 'no_first_bad'}, ensure_ascii=False))
        return
    rows_fb = [r for r in rows if r.get('tag') in tags]
    metrics = compute_metrics(rows_fb)
    metrics['profile'] = args.profile
    out = os.path.join(args.outdir, f'metrics_first_bad_{args.profile or "all"}.json')
    with open(out, 'w', encoding='utf-8') as wf:
        json.dump(metrics, wf, ensure_ascii=False, indent=2)
    print(json.dumps(metrics, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
