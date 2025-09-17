# Teacher Value Domain と Classic Distillation 指針

本ドキュメントでは `train_nnue` ツールにおける **教師値 (teacher value)** の「ドメイン」概念と、Classic NNUE への知識蒸留 (Knowledge Distillation) 仕様・利用法を整理します。

## 背景

Single (大きめ) NNUE から Classic (小さめ) NNUE へ性能を移転する際、教師ネットワークが出力する値がどのスケール / 意味空間 (以下ドメイン) にあるかを明示しないと、混合損失 (教師値とラベルのブレンド) の重み付けや勾配スケールが崩れ、
- 学習初期に極端なロス勾配が発生
- 学習後の MAE / P95 指標が悪化
- 量子化後の精度が不安定
といった問題が発生します。

これを避けるため CLI フラグ `--teacher-domain` を導入し、教師出力の正規化を明示的に行います。

## TeacherValueDomain Enum

| Variant | CLI 値 | 意味 | 想定教師出力 raw | 学習内部での解釈 |
|---------|--------|------|------------------|------------------|
| `Cp` | `cp` | センチポーン (評価値) | 例: +300 / -1200 | `raw` を cp として扱い、logit が必要な場合 `raw / scale` |
| `WdlLogit` | `wdl-logit` | 勝率ロジット (σ 前) | 例: 0.0 (50%), +2.0 (~88%) | そのまま logit として使用。cp 必要時 `raw * scale` |

`scale` は `config.scale` (学習ラベル種類ごと) で与えられます。通常: `wdl` ラベル時は勝率 logit⇔cp の相互変換、`cp` ラベル時は cp 正規化で logit 近似空間を共有します。

## 変換関数

Rust 実装 (`distill.rs` 内) より:

```rust
fn teacher_logit_from_raw(label_type: &str, domain: TeacherValueDomain, raw: f32, scale: f32) -> f32 {
    if label_type == "cp" { return raw / scale; }
    match domain { TeacherValueDomain::WdlLogit => raw, TeacherValueDomain::Cp => raw / scale }
}

fn teacher_cp_from_raw(label_type: &str, domain: TeacherValueDomain, raw: f32, scale: f32) -> f32 {
    if label_type == "cp" { return raw; }
    match domain { TeacherValueDomain::Cp => raw, TeacherValueDomain::WdlLogit => raw * scale }
}
```

### 数式表現

スケール係数 \( S = scale \) とし、教師 raw 出力 \( T_{raw} \) から logit 空間 \( T_{logit} \)、cp 空間 \( T_{cp} \) への変換:

1. 教師ドメイン = cp:
   - \( T_{cp} = T_{raw} \)
   - \( T_{logit} = T_{raw} / S \)
2. 教師ドメイン = wdl-logit:
   - \( T_{logit} = T_{raw} \)
   - \( T_{cp} = T_{raw} * S \)
3. ラベルタイプ label_type = cp の場合は教師 raw は常に cp とみなし、`TeacherValueDomain` は内部 logit 評価指標計算以外には影響を与えない（現在は MSE のみ許容）。

### 勝率 (prob) 関連

logit から勝率への変換は標準シグモイド:
\[ p = \sigma(T_{logit}) = \frac{1}{1 + e^{-T_{logit}}} \]

## 蒸留時のブレンド目標

WDL ラベルの場合 (BCE/MSE/KL):
\[
 p_{teacher} = \sigma(T_{logit} / T_{temp}) , \quad p_{label} = label\\
 p_{target} = \alpha p_{teacher} + (1-\alpha) p_{label}
\]
- \( T_{temp} \): `--kd-temperature` (デフォルト 2.0 推奨、cp ラベル時は 1.0 固定)
- \( \alpha \): `--kd-alpha`

cp ラベルの場合 (MSE のみ):
\[
 v_{teacher} = T_{cp}^{(normalized)} , \quad v_{target} = \alpha v_{teacher} + (1-\alpha) v_{label}
\]
ここで `normalized` は教師ドメインが logit のとき \( T_{cp} = T_{raw} * S \) による cp 化。最終的に MSE を出力 (Classic FP32) と \( v_{target} \) の間で計算します。

## KL 損失

`--kd-loss=kl` 指定時 (WDL ラベルのみ):
\[
 \text{KL}(P_t || P_s) = P_t\log\frac{P_t}{P_s} + (1-P_t)\log\frac{1-P_t}{1-P_s}
\]
現在の実装では教師確率 \(P_t\) と学生確率 \(P_s\) を温度適用後のシグモイド出力として直接比較し、その勾配を student logit へバックプロパゲーションします。ブレンドは KL 項と (必要に応じ) MSE/BCE ではなく、単一選択です。

## クリッピング (cp ラベル蒸留)

極端な教師値に引きずられて学習が不安定化するのを避けるため、cp ラベル蒸留では `--cp-clip` (設定が存在する場合) により教師 cp を \([-C, C]\) にクリップ後ブレンドします。現在 Classic distill 実装では `config.cp_clip` を参照し、`None` の場合は無制限です。

## 評価メトリクス (distill_eval)

構造化ログ `phase=distill_eval` に出力される主な指標:
- `mae_cp`, `p95_cp`, `max_cp`, `r2_cp`
- (WDL時) `mae_logit`, `p95_logit`, `max_logit`
- `n`: 評価サンプル数 (上限 `MAX_DISTILL_SAMPLES`)

ルール:
- cp ラベルでも内部 logit 正規化を行い logit MAE を計算する場合がある（教師/学生比較用途）。
- サンプルは重み > 0 のものから最大 `MAX_DISTILL_SAMPLES` 件。

## ゲーティング (品質閾値)

CLI オプション:
- `--gate-distill-cp-mae <f>`: cp MAE が閾値超過なら失敗 (exit code 1)
- `--gate-distill-cp-p95 <f>`: cp P95 超過で失敗
- `--gate-distill-logit-mae <f>`: (WDLのみ) logit MAE 超過で失敗

CI 等で品質を自動検証する際に利用します。

## CLI 使用例

```bash
# 典型: WDL ラベル + 教師 logit 出力 (推奨)
cargo run -p tools --bin train_nnue -- \
  -i runs/data.cache.gz -e 1 -b 32768 \
  --distill-from-single runs/single_best.fp32.bin \
  --teacher-domain wdl-logit --kd-loss kl --kd-temperature 2.0 --kd-alpha 0.9 \
  --gate-distill-logit-mae 0.15

# 教師が cp 評価を出す旧モデルからの蒸留
cargo run -p tools --bin train_nnue -- \
  -i runs/data.cache.gz -e 1 -b 32768 \
  --distill-from-single runs/legacy_cp.fp32.bin \
  --teacher-domain cp --kd-loss bce --kd-temperature 2.0 --kd-alpha 0.8

# cp ラベルデータ (教師指定必須 / loss=mse 固定)
cargo run -p tools --bin train_nnue -- \
  -i runs/cp_labels.cache.gz -e 1 -b 32768 \
  --distill-from-single runs/single_cp.fp32.bin \
  --teacher-domain cp --kd-alpha 0.7
```

## ベストプラクティス

1. ラベル種別 = wdl の場合は教師を `wdl-logit` で与え、`KL` か `BCE` を検証し精度/安定性で選択する。
2. 学習初期で `mae_logit` が高止まりする場合、`--kd-temperature` を上げる (例: 2.0 → 2.5)。
3. cp 教師を無変換で使うと logit としてスケール不一致を起こすため、必ず `--teacher-domain cp` を明示。
4. ブレンド係数 `--kd-alpha` は 0.8~0.95 が多くの経験で安定。低すぎると教師 benefit が減少し、1.0 近すぎるとラベル分布との乖離で汎化が落ちる。
5. 量子化前後ギャップが想定以上に大きい場合、distill 前に教師/学生 cp MAE を確認しノイズサンプルを除外するフィルタを検討。

## 回帰テスト指針

- 変換関数: logit↔cp 往復/境界値 (±S * 8.0 など) に対し NaN/Inf が出ないこと。
- KL/BCE: p=0,1 近傍の安定性 (clamp epsilon = 1e-6 など) を今後導入予定。
- cp クリップ: クリップ前後で target が閾値以内に収まることを assert。

## 今後の拡張候補

- per-sample 温度 (教師不確実性に応じた adaptive temperature)
- 2-class 以外 (e.g. multi-bucket WDL) への一般化
- ログに教師/学生 分散比を追加してスケール逸脱を早期検出
- 教師 domain 自動推定 (値分布とラベル種からヒューリスティック判定)

---
