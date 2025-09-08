# NNUE 教師データ生成ツールガイド（generate_nnue_training_data）

本書は `crates/tools` の `generate_nnue_training_data` の運用ガイドです。実行オプション、出力（JSONL/manifest v2）、分割/圧縮、構造化ログ、K=3 再探索指標をまとめます。

## 概要
- 入力: SFEN を含むテキスト（1行に1局面）。
- 出力: 学習用 JSONL（またはテキスト）と `manifest.json`（v2）。
- 目的: NNUE 学習に用いる教師データの安定生成・再現性確保・実行状況の可視化。

## 主要オプション
- 予算: `--time-limit-ms <ms>` もしくは `--nodes <n>` のいずれか（同時指定は nodes 優先）。
- エンジン: `--engine {material|enhanced|nnue|enhanced-nnue}`、`--teacher-profile {safe|balanced|aggressive}`。
- 並列: `--jobs <n>`（外側並列、エンジンスレッドは常に1）。
- 出力形式: `--output-format {jsonl|text}`（既定: text）。
- 分割/圧縮: `--split <N>`、`--compress {gz|zst}`（zst は `--features zstd` 必須）。
- 構造化ログ: `--structured-log <PATH|->`（`-` 指定で STDOUT へ JSONL）
- 再探索/K=3: `--amb-gap2-threshold <cp>`、`--amb-allow-inexact`、`--entropy-mate-mode {exclude|saturate}`、`--entropy-scale <f64>`。
- レジューム: `[resume_from]`（位置指定）+ 自動 `*.progress` の両輪で重複防止。

## 推奨コマンド例
- Time モード（最小動作確認）
  ```bash
  cargo run -p tools --bin generate_nnue_training_data -- \
    start_sfens_ply24.txt runs/out_time.jsonl 2 30 0 \
    --engine enhanced --min-depth 2 --time-limit-ms 300 \
    --output-format jsonl --jobs 1 --structured-log runs/logs/out_time.jsonl
  ```
- Nodes モード
  ```bash
  cargo run -p tools --bin generate_nnue_training_data -- \
    start_sfens_ply24.txt runs/out_nodes.jsonl 2 30 0 \
    --engine enhanced --min-depth 2 --nodes 200000 \
    --output-format jsonl --jobs 1 --structured-log runs/logs/out_nodes.jsonl
  ```
- 構造化ログを STDOUT、人間可読ログを STDERR（分離）
  ```bash
  cargo run -p tools --bin generate_nnue_training_data -- \
    start_sfens_ply24.txt runs/out_stdout.jsonl 2 20 0 \
    --engine enhanced --min-depth 1 --time-limit-ms 200 \
    --output-format jsonl --structured-log - \
    1> runs/logs/structured.jsonl 2> runs/logs/human.log
  ```
- 分割/圧縮（part manifest と親 manifest）
  ```bash
  cargo run -p tools --bin generate_nnue_training_data -- \
    start_sfens_ply24.txt runs/out_parts.jsonl 2 100 0 \
    --engine enhanced --min-depth 2 --time-limit-ms 300 \
    --output-format jsonl --split 200 --compress gz \
    --structured-log runs/logs/out_parts.jsonl
  ```

## 構造化ログ（JSONL）
- レコードには `version: 1` を付与。
- `--structured-log -` のとき、JSON は STDOUT に、人間可読ログは STDERR に出力（混在しない）。
- 代表スキーマ（抜粋）
  - `{ "kind":"batch", "version":1, "batch_index":N, "size":M, "success":K, "elapsed_sec":... }`
  - `{ "kind":"final", "version":1, "summary":{...} }`
- `percent` は「成功 / 全入力」の進捗率。将来 `attempted_percent` を追加予定（任意）。

## JSONL（学習データ）出力項目（抜粋）
- 共通: `sfen`, `eval`, `label`, `lines[] {move,score_cp,bound,depth,seldepth,pv,...}`
- K=3 関連（新規）
  - `lines_origin`: `"k2" | "k3"`（採用ラインの由来）
  - `softmax_entropy_k3`: K=3 の3手候補に基づくエントロピー
  - コスト内訳: `time_ms_k2`, `time_ms_k3`, `search_time_ms_total`（合計）
  - ノード内訳: `nodes_k2`, `nodes_k3`, `nodes_total`（合計）

## manifest v2（集約情報）
- 仕様の詳細は [reference/manifest_v2.md](../reference/manifest_v2.md) を参照。
- 重要な運用上の前提
  - `summary` は**今回 run（増分）**の要約。
  - `manifest_scope`: `"part" | "aggregate"`（part は `summary=null`、親に集約 `summary`）。
  - part では `count=count_in_part` のみ信頼。`attempted/skipped/errors` などの集計は親 manifest を参照。

## summary の意味（重要）
- `summary.throughput`
  - `attempted_sps = attempted_run / elapsed_sec`
  - `success_sps = success_run / elapsed_sec`
- `summary.rates`（0..1 でクランプ）
  - `timeout` は**スキップ対象のオーバーラン**（skip_overrun）の比率（`skipped_timeout / attempted_run`）。
  - `top1_exact = success_run / attempted_run`
  - `both_exact = both_exact_count / lines_ge2`
- `summary.counts`
  - `attempted` と `success` は run 増分。`skipped_timeout` と `errors` は今回分のカウント。
- 補足: 累積統計が必要な場合は、manifest トップレベルの `attempted/skipped_timeout/errors` を参照。

## K=3 再探索
- 条件: `gap2 <= --amb-gap2-threshold`（既定25cp）。`--amb-allow-inexact` で非Exactも対象。
- time モードの K=3 追加予算: `min(rem, limit/4)` に **下限20ms** を適用。
- 追加の要約指標
  - `summary.ambiguous.reran`: K=3 を実際に走らせた件数
  - `summary.ambiguous.with_entropy`: エントロピーを算出できた件数（3本揃い・mate処理方針に依存）

## レジューム（重複ゼロ）
- `resume_from` と `*.progress`（試行件数）を見て自動スキップ。
- `.progress` の方が大きければ「既に試行したが失敗/スキップ」分として差分を通知。
- 単一ファイル追記時は改行末尾の整合性を自動補正。

## 互換性
- スキーマは後方互換: 新規項目は Optional。`manifest_version=2` のまま拡張。

