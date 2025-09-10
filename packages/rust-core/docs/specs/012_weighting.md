# Spec 012 — サンプル重み運用

目的: どこを効かせるかのレバー（EXACT/gap/phase/mate）を仕様化し、Gate/レポートまで一貫させる。

## 係数の適用順序
1. `exact`（完全読みの重み）
2. `gap`（`best2_gap` 小さいほど↑）
3. `phase`（opening/middlegame/endgame）
4. `mate`（詰みリング近傍）

- 上記を順序適用し、最終係数は正規化（平均=1.0 目安）

## CLI/設定ファイル
- CLI 例: `--weighting scheme --w-exact 1.5 --w-gap 1.2 --w-phase-endgame 1.3 --w-mate-ring 2.0`
- 設定ファイル: YAML/JSON（例: `balanced.yml`, `endgame_heavy.yml`）
- 競合時の優先度: CLI 明示が最優先、次に設定ファイル、既定値の順

## Gate/レポート項目
- Gate レポートに使用した係数とプリセット名を必ず焼き込む
- `docs/schemas/gauntlet_out.schema.json` に `training_config` セクションで記録可能に

## テスト
- 既定（すべて 1.0）と比較し再現性が崩れないこと
- 係数変更で `val_auc` や勝率に有意差が出る（±方向含む）ことを確認
