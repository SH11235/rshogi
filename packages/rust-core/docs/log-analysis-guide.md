# USIログ分析ワークフロー（外部GUI/サーバ向け）

このガイドでは、**外部 GUI やサーバから取得した USI ログ**（`taikyoku-log/*.md` など）や  
gauntlet の `moves.jsonl` を対象に、評価スパイク（落下局面）を抽出し、再現・再解析する標準手順をまとめます。
主に `scripts/analysis/*.py` / `*.sh` による Python ベースの既存フローを前提とします。

## 1. 事前準備
- 作業ディレクトリ: `packages/rust-core`
- エンジン: `target/release/engine-usi`（必要に応じて `cargo build -p engine-usi --release`）
- `rg` を使うスクリプトが多いので、空振りで止まらないラッパを用意（任意）
  ```bash
  rg(){ command rg "$@" || true; }; export -f rg
  ```

## 2. 概況サマリ（CSV）
- スクリプト: `scripts/analysis/analyze_usi_logs.sh`
- 目的: 最大深さ・seldepth・PV 切替・締切回数の概況把握
- 実行例:
  ```bash
  bash scripts/analysis/analyze_usi_logs.sh taikyoku-log/taikyoku_log_YYYYMMDDHHMM.md \
    | tee runs/diag-$(date +%Y%m%d)/summary.csv
  ```

## 3. 評価スパイク抽出（Python）
- スクリプト: `scripts/analysis/extract_eval_spikes.py`
- 主なオプション: `--threshold`, `--back`, `--forward`, `--topk`, `--out`
- 実行例:
  ```bash
  python3 scripts/analysis/extract_eval_spikes.py \
    --threshold 200 --back 4 --forward 2 --topk 6 \
    --out runs/diag-$(date +%Y%m%d)-spikes \
    taikyoku-log/taikyoku_log_YYYYMMDDHHMM.md
  ```

## 4. 事前手数リプレイ（Python）
- スクリプト: `scripts/analysis/replay_multipv.sh`
- 目的: 指定した手数プレフィクスの局面を MultiPV で再現し、ベストムーブと info を収集
- 実行例:
  ```bash
  PREF=$(cat runs/diag-20251110-1854_spikes/prefixes.txt)
  bash scripts/analysis/replay_multipv.sh taikyoku-log/taikyoku_log_YYYYMMDDHHMM.md \
    -p "$PREF" -o runs/game-postmortem/$(date +%Y%m%d)-5s \
    -t 8 -m 1 -b 5000 --profile match
  ```

## 5. ターゲット生成（Python）
- スクリプト1: `scripts/analysis/extract_positions_from_log.py`
- スクリプト2: `scripts/analysis/expand_targets_back.py`
- 目的: スパイク手から数手遡った `pre_position` を `targets.json` にまとめる
  ```bash
  python3 scripts/analysis/extract_positions_from_log.py \
    --log taikyoku-log/taikyoku_log_YYYYMMDDHHMM.md \
    --out runs/diag-YYYYMMDD/targets.json

  python3 scripts/analysis/expand_targets_back.py \
    --in runs/diag-YYYYMMDD/targets.json \
    --out runs/diag-YYYYMMDD/targets_back.json \
    --min 2 --max 5
  cp runs/diag-YYYYMMDD/targets_back.json runs/diag-YYYYMMDD/targets.json
  ```

## 6. 指標と A/B 評価（first_bad / avoidance）

外部 USI ログから作ったターゲットセット（`targets.json`）を使って、  
探索パラメータの A/B 比較や「真の悪手をどれだけ避けられているか」を測る指標を計算するためのフローです。

### 6.1 用語と指標の概要

- **first_bad（最初の悪手）**  
  - 落下点ログから back 2〜6 手前までの局面をターゲットとして再評価したとき、  
    「ある origin（元ログの落下セグメント）に対して最初に大きくマイナス評価になる局面」を first_bad と呼びます。  
  - どの back 手が first_bad かはスクリプト側で自動判定されます。

- **avoidance_rate（悪手回避率）**  
  - first_bad に対応する各局面について、元ログで実際に指された“悪手”とは違う手を候補エンジンが選べた割合です。  
  - 「悪手を別の手に差し替えられているか」を見る、最重要の指標です。

- その他の指標（補助的）
  - **first_bad 限定スパイク率**: first_bad タグに対して、再評価でも大きなマイナススパイクが出る割合。  
    採否の主指標には使わず、「どのくらい深く落ちるか」の症状を見る用途です。
  - **overall スパイク率**: データセット全体に対するスパイク割合。大幅悪化がないかの監視用。
  - **avg_depth / NPS**: 深さやノードレートの平均。副作用として探索が浅くなっていないかを確認します。

### 6.2 代表的なスクリプト

- `scripts/analysis/pipeline_60_ab.sh`  
  - 役割: ログ → スパイク抽出 → back 生成 → 60 件前後に絞り込み → `run_eval_targets.py` で再評価、までをワンコマンドで実行。
  - 出力: `<out>/targets.json`, `<out>/summary.json`, 各種 CSV/サマリ。

- `scripts/analysis/run_ab_metrics.sh`  
  - 役割: 既存データセット（`targets.json`）に対して、複数のパラメータプリセットを直列評価し、  
    overall / first_bad / avoidance のメトリクスを JSON で出力。

- `scripts/analysis/summarize_first_bad_metrics.py` / `summarize_avoidance.py`  
  - 役割: `summary.json` と `targets.json` から first_bad 限定スパイク率や悪手回避率を集計する補助スクリプト。

### 6.3 コマンド例（60 件データセット + A/B 比較）

1. データセット作成（60 件・10 秒想定）

   ```bash
   ENGINE_BIN=target/release/engine-usi \
   scripts/analysis/pipeline_60_ab.sh \
     --logs 'taikyoku-log/taikyoku_log_enhanced-parallel-202511*.md' \
     --out runs/$(date +%Y%m%d-%H%M)-tuning \
     --threads 8 \
     --byoyomi 10000
   ```

2. A/B 評価（複数プリセットをまとめて比較）

   ```bash
   scripts/analysis/run_ab_metrics.sh \
     --dataset runs/20251112-2014-tuning \
     --out-root runs/$(date +%Y%m%d-%H%M)-ab \
     scripts/analysis/param_presets/f1e47_lmp_mid.json \
     scripts/analysis/param_presets/f1e47_lmp_mid_lmr200.json
   ```

   - 各プリセット配下に `metrics.json`（overall）, `metrics_first_bad.json`（first_bad 限定）, `avoidance.json`（悪手回避率）のような指標ファイルが生成されます。
   - 所要時間の目安: 60 件 × 10 秒 ≒ 1 プリセットあたり 16 分前後。3 案を直列で回すとおよそ 50 分程度です。

### 6.4 実行時の注意点

- 1 つのデータセット配下で `run_eval_targets.py` を多重実行しないでください（`summary.json` の書き込み競合を避けるため）。
- `Finalize` / `MateGate` / `InstantMate` 系のオプションは「診断用の安全弁」であり、  
  A/B 採否のための主な改善策としては扱わず、探索ロジックや時間管理の根本的な改善を優先してください。

## 7. Selfplay ログについて

`selfplay_basic` が出力する Selfplay ログ（`runs/selfplay-basic/*.jsonl` + `.info.jsonl`）の解析は、  
Python スクリプトではなく Rust 製 CLI（`selfplay_basic` / `selfplay_blunder_report` / `selfplay_eval_targets`）を用いた別フローにまとめています。

Selfplay ログを使った自己対局＋ブランダー分析の手順については、次の専用ドキュメントを参照してください。

- [`docs/selfplay-basic-analysis.md`](./selfplay-basic-analysis.md)
