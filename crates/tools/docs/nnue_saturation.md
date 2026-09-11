# nnue_saturation

LayerStacks NNUE の**活性飽和率**を実局面で計測する診断ツールです。ClippedReLU /
SqrClippedReLU 系の活性は u8 [0,127] に clamp されるため、127 到達率が高いほど
量子化天井で情報が落ちています（評価値インフレ → 隠れ層飽和、の計器）。

計測する 3 段:

| 段 | 内容 |
|---|---|
| `ft` | 推論に渡す FT 因子 clamp(piece + Threat, 0, 127) の 127 到達率（両視点 2×L1 因子。SqrClippedReLU の出力は `(a*b) >> 7` で最大 126 のため、飽和は pairing 前の因子側で観測する） |
| `l1_act` | L1→L2 activation（SqrClippedReLU + ClippedReLU の 2×main_dim 要素） |
| `l2_act` | L2→output activation（ClippedReLU の 32 要素） |

**重み側**（i8 dense weight の ±127 張り付き率、FT i16 の飽和接近）は rshogi-nnue
(tatara) の `crates/nnue-format/examples/clamp_stats.rs` が担当します。本ツールは
局面依存の活性側のみを扱います。

Threat が有効なモデルでは、実際の forward と共通の処理で piece accumulator と
Threat accumulator を i16 の wrapping 和で合成します。この合成後の値を `ft` の因子計数と
`l1_act` / `l2_act` の dense 入力の両方に使います。Threat が無効なら piece のみです。
PSQT は別の評価加算経路なので、これらの活性計器には含めません。

## 使い方

```bash
cargo run -p tools --release --bin nnue_saturation -- \
  --nnue "$SHOGI_DATA/nnue/model.bin" \
  --progress-coeff "$SHOGI_DATA/progress/progress.bin" \
  --progress-buckets 8 \
  --sfens sfens.txt \
  --out saturation.json
```

Threat モデルには `--features nnue-threat` を追加し、モデルと同じ Threat profile の
feature を選んでビルドしてください。PSQT 併用モデルには `nnue-psqt` も必要です。
対応する L1 サイズ・FT 種類の静的ビルド設定もモデルに合わせます。

`--progress-buckets` は学習時の routing bucket 数を指定します。`--progress-buckets 1` は
常に bucket 0 を選ぶ no-op routing のため、その場合のみ `--progress-coeff` は省略できます。

出力は JSON（標準出力と `--out`）。全体集計 `total` と、progress bucket 別の
`per_bucket`（局面が 1 件以上入った bucket のみ）を含みます。各段は
`*_sat` / `*_total` / `*_rate`（127 到達数 / 総数 / 率）です。

入玉局面（大評価値側）と一般局面で `--sfens` を替えて比較すると、評価値インフレが
どの帯で天井に当たっているかを切り分けられます。bucket は progresskpabs なので、
終盤 bucket ほど入玉局面が集中します。

## 過去の計測との比較

Threat 合成に対応する前のツールは piece のみで活性を計測していました。同じ JSON 項目名でも、
Threat モデルの旧計測をそのまま合成後の値と比較しないでください。元の実行 binary の commit・
build feature・モデルと局面集合を確認し、同条件で再計測して差を判断します。
過去の数値を現在のソースだけから無効と断定したり、自動的に置き換えたりはしません。
