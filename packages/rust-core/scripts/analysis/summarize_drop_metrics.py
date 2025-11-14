#!/usr/bin/env python3
import argparse
import json
import os


def main():
    ap = argparse.ArgumentParser(description='summary.json から落下率等の指標を算出')
    ap.add_argument('outdir')
    ap.add_argument('--profile', default=None, help='特定プロファイル名のみを見る（未指定で全件平均）')
    ap.add_argument('--bad-th', type=int, default=-600, help='bad判定のCP閾値（既定:-600）')
    args = ap.parse_args()

    rows = json.load(open(os.path.join(args.outdir,'summary.json'),'r',encoding='utf-8'))
    if args.profile:
        rows = [r for r in rows if r.get('profile')==args.profile]
    total = len(rows)
    bad = 0
    cp_sum = 0
    depth_sum = 0
    valid = 0
    for r in rows:
        cp=r.get('eval_cp')
        d=r.get('depth',-1)
        if cp is not None:
            valid += 1
            cp_sum += cp
            if cp <= args.bad_th:
                bad += 1
        if isinstance(d,int) and d>=0:
            depth_sum += d

    avg_cp = (cp_sum/valid) if valid else None
    avg_depth = (depth_sum/valid) if valid else None
    spike_rate = (bad/valid*100.0) if valid else None
    out = {
        'profile': args.profile,
        'total': total,
        'valid': valid,
        'bad_th': args.bad_th,
        'bad_count': bad,
        'spike_rate_percent': spike_rate,
        'avg_cp': avg_cp,
        'avg_depth': avg_depth,
    }
    print(json.dumps(out, ensure_ascii=False, indent=2))

if __name__=='__main__':
    main()

