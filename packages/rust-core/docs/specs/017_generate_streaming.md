# Spec 017 — 生成ストリーミング & expected-multipv auto

目的: 大規模 SFEN 入力を逐次処理し、ピークメモリを一定化。manifest による `expected-multipv` 自動解決を定義。

## ストリーミング設計
- 入力: stdin/pipe/ファイルいずれも `BufRead` 逐次処理
- バッファ: 64–256 KiB 目安（実測で調整）
- イテレータ境界: 行単位（SFEN 1 行 1 局面）

## メモリ測定
- `/proc/self/status` の `VmHWM` を読み取り（Linux）
- 代替: `time -v` の `Maximum resident set size`
- 回帰テスト（目安）:
  - 入力行数 `{1, 1e5, 1e6}` で `VmHWM` が一定 ±X%（X は 10–20% 目安）

## expected-multipv auto
決定ロジック（優先度の高い順）
1. CLI 明示（常に最優先で上書き）
2. `final.manifest` の `aggregated.multipv`
3. 集約 manifest（top-level `multipv`, 例: `manifest_scope=aggregate`）
4. 既定値 = `2`

受理する manifest 形状（後方互換）
- `aggregated.multipv`（例: `{ "aggregated": { "multipv": 3 } }`）
- top-level `multipv`（集約 manifest; 例: `{ "multipv": 3, "manifest_scope": "aggregate" }`）
どちらも無い/manifest 不在時は上記の決定ロジックに従い既定値 `2` を使用（CLI 数値は常に最優先）

## 後方互換シナリオ
- 既存 manifest v2 と互換（スキーマ: `docs/schemas/manifest_v2.schema.json`）
- CI に小回帰テスト追加（小/中/大入力、error-handling）

## Fixtures
- 入力サンプル: `docs/reports/fixtures/psv_sample.psv`
- 使用例（stdin パイプ）:
  ```sh
  cat docs/reports/fixtures/psv_sample.psv | \
    cargo run -p tools --bin generate_nnue_training_data -- \
      --engine enhanced --min-depth 1 --time-limit-ms 100 --hash-mb 256 --multipv 1
  ```
