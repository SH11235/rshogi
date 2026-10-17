# eval_sfens

SFEN テキスト（1 行 1 局面）を LayerStacks NNUE で静的評価し、TSV を標準出力へ出す。

```sh
cargo run --release -p tools --bin eval_sfens -- \
  --nnue "$SHOGI_DATA/nnue/<model>.bin" \
  --sfens sfens.txt \
  --progress-coeff "$SHOGI_DATA/progress/<coeff>.bin" \
  --progress-buckets 8 \
  --count 10
```

`--bucket-mode` は `progresskpabs`（既定）。`--progress-buckets` は必須で、
モデルの bucket 数と routing 設定に合わせる。`--count` の既定値は 10 で、
入力の先頭から読む行数を制限する（空行も数えるが評価はしない）。
出力列は `sfen`、`bucket`、`raw`、`score`（歩=90 の内部スケール）、
`score_cp`（歩=100 の cp）。

`--dump-debug-first` で最初の評価局面の中間値を標準エラーへ出す。
L1 の診断は読み取り専用の重みを使い、従来の密 SIMD 計算とスカラー計算を比較する。

### L2=64 の専用モデル

静的 LayerStacks の解析には、モデルの寸法に合わせて
`--features layerstacks-3072x16x64`、`--features layerstacks-2304x16x64` または `--features layerstacks-1536x16x64`
をビルド時に追加する。L2 の中間出力も指定した幅で計算する。
