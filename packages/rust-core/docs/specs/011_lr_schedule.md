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
  - 学習: `lr`, `examples_sec`, `loader_ratio`
  - 検証: `val_loss`, `val_auc`
  
  備考: `train_loss` は任意。直近のミニバッチで損失が未計算（例: 全サンプルが weight=0 のバッチを通過した直後など）のスループット行では省略される場合があります。スキーマ（`docs/schemas/structured_v1.schema.json`）とも整合します。
- スキーマ: `docs/schemas/structured_v1.schema.json`

例（1 行）
```json
 {"ts":"2025-09-10T12:34:56Z","phase":"train","global_step":1200,"epoch":3,"lr":0.00083,"train_loss":0.642,"examples_sec":9350.4,"loader_ratio":0.91,"wall_time":123.4}
```

## ステップ意味論（global_step と LR 減衰）

- 定義: `global_step` は「完了済みバッチ数」。学習ループにおいて各バッチ処理の完了直後に必ず +1 されます。
- ゼロ重み: バッチ内サンプルの重み合計が 0 の場合でも、`global_step` は前進します（計算のスキップは行いますが、ステップは進む）。
- 一貫性: 上記は in‑memory／stream‑cache、sync／async の全経路で共通です。
- スケジュール: `--lr-decay-steps` を指定した場合、減衰の進行は `global_step` に基づきます。`--lr-decay-epochs` はエポック数に基づきます。Warmup も同様に `global_step`/エポックのいずれかで適用されます（実装依存の分岐に従う）。
- 互換性: 現仕様では「有効更新バッチ数」（非ゼロ重みバッチのみをカウント）による減衰は行いません。将来的にオプトインのオプションを追加する場合は別途明記します。

## Plateau（任意オーバーレイ）

- 有効条件: `--lr-plateau-patience N`（N>0）かつ検証データが提供されている場合のみ有効。
- 監視対象: 各エポック末の `val_loss`。`NaN/Inf` の場合は更新をスキップし警告のみ。
- 係数: 既存スケジュールの係数に対し、`multiplier` を乗算（初期値1.0）。改善なしが `patience` 連続すると `multiplier *= 0.5`。
- 適用タイミング: 次エポックの全バッチに一律で適用。
- ログ: 人間可読ログに `LR plateau: epoch N → lr *= 0.5 (multiplier now X, best=..., cur=...)` を1行出力。structured_v1 の `lr` は合成後の値（plateau反映済）。

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
