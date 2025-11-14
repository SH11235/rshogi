#!/usr/bin/env python3
import argparse
import json
import os
from collections import defaultdict


def load_targets(outdir):
    with open(os.path.join(outdir, 'targets.json'), 'r', encoding='utf-8') as f:
        data = json.load(f)
    return data.get('targets', [])


def load_results(outdir):
    with open(os.path.join(outdir, 'summary.json'), 'r', encoding='utf-8') as f:
        rows = json.load(f)
    # rows: [{tag, profile, eval_cp, depth}]
    res = defaultdict(dict)
    for r in rows:
        res[r['tag']][r['profile']] = {'cp': r.get('eval_cp'), 'depth': r.get('depth')}
    return res


def main():
    ap = argparse.ArgumentParser(description='targets.json と summary.json を突き合わせ、真の悪手候補を集計')
    ap.add_argument('outdir', help='run_eval_targets.py の出力ディレクトリ（targets.json/summary.json がある場所）')
    ap.add_argument('--bad-th', type=int, default=-300, help='bad判定のCP閾値（既定:-300）')
    args = ap.parse_args()

    targets = load_targets(args.outdir)
    results = load_results(args.outdir)

    # tag -> base/gates/rootfull
    records = []
    by_origin = defaultdict(list)
    for t in targets:
        tag = t['tag']
        r = results.get(tag, {})
        base = r.get('base', {})
        gates = r.get('gates', {})
        rootfull = r.get('rootfull', {})
        rec = {
            'tag': tag,
            'origin': f"{t.get('origin_log','')}:{t.get('origin_ply','')}",
            'back': t.get('back_plies'),
            'base_cp': base.get('cp'),
            'gates_cp': gates.get('cp'),
            'rootfull_cp': rootfull.get('cp'),
            'base_depth': base.get('depth'),
            'gates_depth': gates.get('depth'),
            'rootfull_depth': rootfull.get('depth'),
        }
        # フラグ付け
        bc = rec['base_cp']; gc = rec['gates_cp']; rc = rec['rootfull_cp']
        sev = None
        if bc is not None and bc <= args.bad_th:
            if (gc is not None and gc > 0) or (rc is not None and rc > 0):
                sev = 'rescue_by_gates_or_rootfull'  # 刈り/ゲート過剰の疑い
            else:
                sev = 'both_bad'
        else:
            sev = 'ok_or_unclear'
        rec['severity'] = sev
        # 改善量（gates/base差の絶対値）
        if bc is not None and gc is not None:
            rec['dg'] = gc - bc
        else:
            rec['dg'] = None
        records.append(rec)
        by_origin[rec['origin']].append(rec)

    # originごとに back 昇順で並べ、最初に bad になる back を拾う
    first_bad = []
    for origin, items in by_origin.items():
        items = [x for x in items if x['back'] is not None]
        items.sort(key=lambda x: x['back'])
        chosen = None
        for x in items:
            if x['base_cp'] is not None and x['base_cp'] <= args.bad_th:
                chosen = x
                break
        if chosen:
            first_bad.append(chosen)

    # CSV 出力
    def wcsv(path, rows, header):
        with open(os.path.join(args.outdir, path), 'w', encoding='utf-8') as wf:
            wf.write(header + '\n')
            for r in rows:
                wf.write(','.join(str(r.get(k,'')) for k in header.split(',')) + '\n')

    # 深刻順（dg降順→back昇順）: ゲート等で救える可能性が高い候補
    rescue_candidates = [r for r in records if r['severity']=='rescue_by_gates_or_rootfull' and r['dg'] is not None]
    rescue_candidates.sort(key=lambda x: (-(x['dg']), x['back']))
    wcsv('true_blunders_rescue_candidates.csv', rescue_candidates,
         'origin,tag,back,base_cp,gates_cp,rootfull_cp,dg,base_depth,gates_depth,rootfull_depth,severity')

    # 最初のbad（originごと）
    first_bad.sort(key=lambda x: (x['origin'], x['back']))
    wcsv('true_blunders_first_bad.csv', first_bad,
         'origin,tag,back,base_cp,gates_cp,rootfull_cp,base_depth,gates_depth,rootfull_depth,severity')

    print(f"wrote {len(rescue_candidates)} rescue_candidates -> {args.outdir}/true_blunders_rescue_candidates.csv")
    print(f"wrote {len(first_bad)} first_bad -> {args.outdir}/true_blunders_first_bad.csv")


if __name__ == '__main__':
    main()

