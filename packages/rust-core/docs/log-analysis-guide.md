# USIログ分析ワークフロー

このガイドでは、USI ログや selfplay ログから評価スパイク（落下局面）を抽出し、再現・再解析する標準手順をまとめます。Python スクリプトでの既存フローと、selfplay 用に整備した Rust CLI を併記しています。

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

## 6. Selfplay ログ向け Rust CLI
selfplay (`runs/selfplay-basic/*.jsonl` + `.info.jsonl`) の解析は Rust CLI を優先する。

1. ブランダー抽出 + ターゲット化
   ```bash
   cargo run -p tools --bin selfplay_blunder_report -- \
     runs/selfplay-basic/<log>.jsonl \
     --threshold 400 \
     --back-min 0 \
     --back-max 3
   ```
   - 出力: `runs/analysis/<log>-blunders/{blunders.json,targets.json,summary.txt}`

2. ターゲット再解析（Multi Profile）
   ```bash
   cargo run -p tools --bin selfplay_eval_targets -- \
     runs/analysis/<log>-blunders/targets.json \
     --threads 8 --byoyomi 2000
   ```
   - `engine-usi` を base/rootfull/gates で再実行し、`summary.json` と `__<profile>.log` を生成

閾値やバック手数のチューニングなど詳細メモは `docs/tuning-guide.md` の「自己対局ログからのクイック診断」を参照。
