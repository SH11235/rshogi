# Spec 011 — 学習率スケジュール & 構造化ログ

目的: 再現可能な LR スケジュール運用と、ダッシュボードで消費可能なログ出力を規定する。

## CLI 仕様
- オプション:
  - `--lr-schedule {constant|step|cosine}`（既定: `constant`）
  - `--lr-warmup-epochs <u32>`（既定: 0）
  - `--lr-decay-epochs <u32>` または `--lr-decay-steps <u64>`（両立不可）
  - `--lr-plateau-patience <u32>`（任意）
- 相互排他/検証:
  - `--lr-decay-epochs` と `--lr-decay-steps` は同時指定不可（エラーコード 2）
  - 未知のスケジュールはエラーコード 2

## 既定値と無効値
- `--lr-schedule` 未指定時は `constant`
- 負の値/ゼロ不許可の項目（epochs/steps）はエラーコード 2

## ログ仕様（structured_v1）
- 出力先: JSONL（1 行 1 レコード）
- 必須キー:
  - 共通: `ts`, `phase`, `global_step`, `epoch`, `wall_time`
  - 学習: `lr`, `train_loss`, `examples_sec`, `loader_ratio`
  - 検証: `val_loss`, `val_auc`
- スキーマ: `docs/schemas/structured_v1.schema.json`

例（1 行）
```json
{"ts":"2025-09-10T12:34:56Z","phase":"train","global_step":1200,"epoch":3,"lr":0.00083,"train_loss":0.642,"examples_sec":9350.4,"loader_ratio":0.91,"wall_time":123.4}
```

## テスト行列
- スケジュール: `{constant, step, cosine}`
- Warmup: `{on, off}`
- 混合精度: `{on, off}`
- 期待:
  - `constant`: 学習中 `lr` 一定
  - `step`: 指定エポック/ステップで段差
  - `cosine`: 区間で滑らかに減衰
  - すべて structured_v1 を満たす

## 失敗時の戻し方
- 既存 run と再現性が崩れた場合は `--lr-schedule constant` に切り戻し、ログに `rollback_reason` を出力（任意）

## Fixtures
- ログ検証サンプル: `docs/reports/fixtures/jsonl_sample.jsonl`
- 検証: `docs/schemas/structured_v1.schema.json` と照合
