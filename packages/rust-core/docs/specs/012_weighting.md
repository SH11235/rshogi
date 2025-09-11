# Spec 012 — サンプル重み運用（Weighting）

目的: どこを効かせるかのレバー（exact/gap/phase/mate）を仕様化し、CLI/設定/ログ/Gate を一貫させる。

## 係数の適用順序（厳守）
1. exact（完全読みの強調）
2. gap（`best2_gap` 小さいほど↑）
3. phase（opening/middlegame/endgame）
4. mate（詰みリング近傍）

- 上記の順序で係数を逐次乗算する。すべて 1.0 のときは従来の重み（ベースライン）と完全一致。

## CLI（tools/train_nnue）
- `--weighting {exact|gap|phase|mate}`（複数指定可）
- 係数指定（既定=1.0）:
  - `--w-exact <NUM>`
  - `--w-gap <NUM>`
  - `--w-phase-endgame <NUM>`（必要に応じて opening/middlegame も将来拡張）
  - `--w-mate-ring <NUM>`

注記: CLI の phase 係数は v1 では endgame のみ指定可。設定ファイルでは opening/middlegame/endgame を個別設定可能だが、CLI 指定が優先される。

例:
```
train_nnue -i train.jsonl --weighting exact --weighting phase \
  --w-exact 1.5 --w-phase-endgame 1.3 --structured-log runs/train.jsonl
```

## 設定ファイル（YAML/JSON, 任意）
- フォーマット例:
```yaml
weighting: [exact, gap, phase, mate]
w_exact: 1.2
w_gap: 1.1
w_phase_endgame: 1.3
w_mate_ring: 2.0
preset: endgame_heavy
```
- 競合時の優先度: CLI > config > 既定（=1.0）
- 拡張子で自動判定（.yaml/.yml=YAML, .json=JSON）。未知キーはエラー（deny_unknown_fields）

## Gate/レポート
- 構造化JSONL（structured v1）の各レコードに `training_config` を含め、使用中のスキームと係数（および `preset` があれば）を焼き込む。
  - 付加情報: `phase_applied: true|false`（入力がJSONLかつ `weighting` に `phase` を含む場合に true）
- Gauntlet 集計（`gauntlet_out.schema.json`）も `training_config` を許容（参考: schema）。

## テスト（DoD）
- 既定: すべて 1.0 でベースライン結果と数値一致（決定論テスト）。
- 優先度: CLI が設定ファイルを上書きすることを確認（パース単体テスト）。
- 適用順: exact → gap → phase → mate の順で係数が乗ること（ユニットテスト）。
- 端末検証: endgame-heavy プリセットで `val_auc` が変化する（最小合成データによる重み付きAUC差）。

### 実装備考（v1）
- 適用順序はコードで正規化（exact → gap → phase → mate）。重複指定は除去される。
- ベースライン重みは `gap` が小さいほど減衰（学習寄与↓）する。一方、`--weighting gap` はベースラインへの“ボーナス”として乗算され、逆転するわけではない。
- gap の強調は v1 では簡易ステップ関数（`gap < 50cp` で `w_gap` を乗算）。将来、連続スケール化を検討。
- ベースラインの gap 重みは `gap=0` でも 0 にならないよう微小下限（例: `1e-3`）を敷いており、レバーが有効に働く。
- `preset` は v1 ではメタデータ（ログ・Gate追跡用）のみで、係数の自動設定は行わない。
