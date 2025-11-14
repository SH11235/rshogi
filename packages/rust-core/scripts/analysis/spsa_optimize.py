#!/usr/bin/env python3
"""
SPSA で探索パラメータを最適化（落下率最小化）。

前提:
- 評価データセット: <outdir>/targets.json（本ツールはそこへ summary.json を都度上書きするので、
  候補ごとに別サブディレクトリを使う）
- 候補評価: run_eval_targets_params.py を呼び出す
- 目的関数: summarize_drop_metrics.py の spike_rate_percent（小さいほど良い）

使い方（例）:
  python3 scripts/analysis/spsa_optimize.py \
    --dataset runs/diag-20251112-tuning \
    --iters 10 --threads 8 --byoyomi 10000 \
    --config scripts/analysis/spsa_example_config.json \
    --work runs/diag-20251112-spsa

config JSON 形式:
{
  "name": "spsa-exp1",
  "bad_th": -600,
  "a0": 0.5, "c0": 1.0, "A": 5, "alpha": 0.602, "gamma": 0.101,
  "params": {
    "LMR_K_x100": {"init": 160, "min": 120, "max": 200, "step": 2},
    "QS_BadCaptureMin": {"init": 400, "min": 300, "max": 600, "step": 10},
    "QS_MarginCapture": {"init": 150, "min": 100, "max": 250, "step": 5}
  },
  "env": {"SHOGI_QUIET_SEE_GUARD": "1"}
}
"""
import argparse, json, math, os, random, shutil, subprocess, sys, time


def clamp_step(x, mn, mx, step):
    x = max(mn, min(mx, x))
    if step and step>1:
        # 近いステップへ丸め
        r = round((x - mn) / step) * step + mn
        r = max(mn, min(mx, r))
        return int(r)
    return int(x)


def eval_candidate(dataset_dir, work_dir, name, params, env, threads, byoyomi, minthink, warmupms, bad_th):
    os.makedirs(work_dir, exist_ok=True)
    # params.json を書き、run_eval_targets_params.py を実行
    pjson = os.path.join(work_dir, 'params.json')
    with open(pjson, 'w', encoding='utf-8') as wf:
        json.dump({'name': name, 'params': params, 'env': env or {}}, wf, ensure_ascii=False, indent=2)
    # targets.json を参照するため、work_dir にシンボリックリンクを張る（なければコピー）
    src_t = os.path.join(dataset_dir, 'targets.json')
    dst_t = os.path.join(work_dir, 'targets.json')
    if not os.path.exists(dst_t):
        try:
            os.symlink(os.path.abspath(src_t), dst_t)
        except Exception:
            shutil.copy2(src_t, dst_t)

    cmd = [sys.executable, 'scripts/analysis/run_eval_targets_params.py', work_dir,
           '--params-json', pjson,
           '--threads', str(threads), '--byoyomi', str(byoyomi), '--minthink', str(minthink), '--warmupms', str(warmupms)]
    subprocess.run(cmd, check=True)

    # メトリクス算出
    cmd2 = [sys.executable, 'scripts/analysis/summarize_drop_metrics.py', work_dir, '--bad-th', str(bad_th)]
    out = subprocess.check_output(cmd2, text=True)
    metrics = json.loads(out)
    return metrics


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--dataset', required=True, help='targets.json のあるディレクトリ')
    ap.add_argument('--work', required=True, help='SPSA の作業ディレクトリ（候補ごとにサブフォルダ作成）')
    ap.add_argument('--config', required=True, help='SPSA 設定JSON')
    ap.add_argument('--iters', type=int, default=10)
    ap.add_argument('--threads', type=int, default=8)
    ap.add_argument('--byoyomi', type=int, default=10000)
    ap.add_argument('--minthink', type=int, default=100)
    ap.add_argument('--warmupms', type=int, default=200)
    args = ap.parse_args()

    cfg = json.load(open(args.config,'r',encoding='utf-8'))
    name = cfg.get('name','spsa')
    bad_th = cfg.get('bad_th', -600)
    A = cfg.get('A', 5)
    a0 = cfg.get('a0', 0.5)
    c0 = cfg.get('c0', 1.0)
    alpha = cfg.get('alpha', 0.602)
    gamma = cfg.get('gamma', 0.101)
    params_cfg = cfg['params']
    env_cfg = cfg.get('env', {})

    # 初期ベクトル
    theta = {k: int(v['init']) for k,v in params_cfg.items()}

    os.makedirs(args.work, exist_ok=True)

    # ベースライン計測
    base_metrics = eval_candidate(args.dataset, os.path.join(args.work, f'{name}_base'), f'{name}_base', theta, env_cfg, args.threads, args.byoyomi, args.minthink, args.warmupms, bad_th)
    print('[SPSA] baseline:', base_metrics)

    for it in range(1, args.iters+1):
        ak = a0 / ((it + A) ** alpha)
        ck = c0 / (it ** gamma)
        # 摂動 delta ∈ {+1,-1}
        delta_sign = {k: (1 if random.random()<0.5 else -1) for k in params_cfg.keys()}
        theta_plus = {}
        theta_minus = {}
        for k, pc in params_cfg.items():
            step = int(pc.get('step',1))
            d = int(round(ck * step)) if step>0 else int(round(ck))
            if d < 1:
                d = 1
            theta_plus[k]  = clamp_step(theta[k] + delta_sign[k]*d, pc['min'], pc['max'], step)
            theta_minus[k] = clamp_step(theta[k] - delta_sign[k]*d, pc['min'], pc['max'], step)

        m_plus = eval_candidate(args.dataset, os.path.join(args.work, f'{name}_it{it}_plus'), f'{name}_it{it}_plus', theta_plus, env_cfg, args.threads, args.byoyomi, args.minthink, args.warmupms, bad_th)
        m_minus= eval_candidate(args.dataset, os.path.join(args.work, f'{name}_it{it}_minus'), f'{name}_it{it}_minus', theta_minus, env_cfg, args.threads, args.byoyomi, args.minthink, args.warmupms, bad_th)

        Jp = m_plus['spike_rate_percent'] or 0.0
        Jm = m_minus['spike_rate_percent'] or 0.0
        # 勾配推定（Jを最小化）
        g = {}
        for k, pc in params_cfg.items():
            step = int(pc.get('step',1))
            d = abs(theta_plus[k] - theta_minus[k])
            d = max(d, 1)
            g[k] = (Jp - Jm) / d
        # 更新
        for k, pc in params_cfg.items():
            newv = theta[k] - ak * g[k] * pc.get('step',1)
            theta[k] = clamp_step(int(round(newv)), pc['min'], pc['max'], int(pc.get('step',1)))

        # 現在点の評価（任意）
        m_cur = eval_candidate(args.dataset, os.path.join(args.work, f'{name}_it{it}_cur'), f'{name}_it{it}_cur', theta, env_cfg, args.threads, args.byoyomi, args.minthink, args.warmupms, bad_th)
        print(f"[SPSA] iter {it}: theta={theta} metrics={m_cur}")

    # 最終レポート
    final_path = os.path.join(args.work, f'{name}_final_theta.json')
    with open(final_path,'w',encoding='utf-8') as wf:
        json.dump({'theta': theta}, wf, ensure_ascii=False, indent=2)
    print(f"[SPSA] wrote final theta -> {final_path}")


if __name__=='__main__':
    main()

