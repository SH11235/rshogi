# NNUE データ生成（generator）FAQ

本FAQは `crates/tools/src/bin/generate_nnue_training_data.rs` が出力する進捗や成功/失敗の意味、よくある疑問を簡潔にまとめたものです。

## 進捗表示の意味

- `Batch X/Y: Processing 256 positions...`
  - 入力から「試行（attempt）」するポジション数の進捗。1バッチは既定で256件。
- `Batch complete: N results in 68.0s (4 pos/sec)`
  - そのバッチで「採用（成功）できたレコード数」。`N` は成功数（JSONLに書かれた件数）。
- `Overall progress: A/B (P%)`
  - 分子`A`: 成功（採用）累計件数。分母`B`: 入力の総件数。
  - つまり「バッチが進んでも成功が少なければ%は伸びない」のが正しい挙動。

## 成功と失敗（スキップ）の基準

- 成功（採用）
  - `lines[0]` が存在し、Top1 の `bound` が `Exact`。必要なメタ（nodes/time/seldepth 等）を付与してJSONLに1行出力。
- 失敗（search_error）
  - `empty_or_missing_pv`: `lines` が空（PVが取れない/不整合）。
  - `nonexact_top1`: `lines[0]` が `Exact` でない（`LowerBound`/`UpperBound`）。
  - `time_overrun`: 1局面の探索時間が `--time-limit-ms × skip_overrun_factor` を超過（既定 factor=2.0）。

## よくある質問

Q. 「256 positions のうち 27 results」は“27件が成功、他は失敗”という理解で良い？

A. はい。成功はJSONLに採用された件数、失敗は `train.manifest.json` の `errors` と `skipped_timeout` に加算されます。

Q. 成功率が低いが、`time_ms` を増やせば解決する？

A. `empty_or_missing_pv` 支配なら、時間延長だけでは限定的。探索側（エンジン）の着地設計（P0/P1）を優先して改善してください（empty≈0%が第一目標）。

Q. 途中から再開したい。

A. `--split`/`--compress`を使わない単一出力なら、自動で追記再開（既存行数をカウントし末尾に追記）。parted出力（`--split` 有効）ではパートごとにファイルが分かれます。

## 関連ファイル

- 生成: `crates/tools/src/bin/generate_nnue_training_data.rs`
- マニフェスト: `*.manifest.json`（集計/サマリ）
- 採用レコード: `out.jsonl` または `out.part-0001.jsonl`（`--split` 有効時）

