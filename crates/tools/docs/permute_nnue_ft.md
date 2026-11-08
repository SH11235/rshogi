# permute_nnue_ft

LayerStacks NNUE の Feature Transformer と各 bucket の第 1 全結合層を同じ順序に並べ替え、評価値を保った新しい標準 `.bin` を作ります。Threat と PSQT を含むモデルは対象外です。入力と同じファイルや既存ファイルには書き込みません。

```bash
cargo run --release -p tools --bin permute_nnue_ft -- \
  --nnue "$SHOGI_DATA/nnue/input.bin" \
  --permutation "$SHOGI_DATA/nnue/permutation.json" \
  --output "$SHOGI_DATA/nnue/permuted.bin"
```

順序ファイルは JSON の整数配列です。FT 幅の半分の要素を含み、各値は変換後の位置に置く変換前の位置を表します。`0` から半幅未満までが重複なく必要です。同じ並べ替えを両方の FT 半分と、各 bucket の L1 入力の両視点に適用します。

FT bias と L1 weight の index は変換後のファイル順になります。元モデル用の `SPSA_NET_ft_b_*` 設定や L1 weight の index はそのまま使えません。元モデルに delta を適用してから変換するか、変換後のモデルで調整してください。出力・L2 の係数 index は変わりません。
