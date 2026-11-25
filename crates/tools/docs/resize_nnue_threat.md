# resize_nnue_threat

LayerStacks の Threat 行を指定幅に縮め、FT・PSQT・全 bucket の FC 係数を保持した新しい `.bin` を作ります。入力と既存出力は上書きしません。標準 `.bin` が対象で、prepacked 形式は入力に使えません。

```bash
cargo run -p tools --no-default-features --bin resize_nnue_threat -- \
  --nnue "$SHOGI_DATA/nnue/1536x16x64-threat-full.bin" \
  --width 256 --output "$SHOGI_DATA/nnue/threat256.bin"
```

`--seed 42` を付けると Threat だけを一様な整数乱数 -4..=4 に置き換えます。同じ入力・幅・seed で出力は一致します。省略時は各行の前半の先頭 T/2 列と後半の先頭 T/2 列を保持します。幅は 128 の倍数かつ入力の Threat 幅以下です。FT や後段の係数を保持しても、幅を縮めると評価値と探索木は変わります。未学習の変換 net は速度試験用です。

入力を全件ロードせず、レイアウト走査と行ごとの変換を行います。変換時のバッファは元の Threat 一行分です。

## ファイル形式と推論契約

- version は入力を保持します。`0x7af32f20` は bucket 数 9 固定、`0x7af32f21` は architecture 文字列直後の little-endian u32 に bucket 数を持ちます。
- 先頭は little-endian u32 の version、network hash、architecture の UTF-8 byte 長、続いて architecture の byte 列です。architecture に `ThreatWidth=T` をカンマ区切り token として追加し、T には変換後の幅を10進数で入れます。省略時は T=L1 です。長さには追加 token を含めます。
- 既存の `Threat=<特徴数>` と `ThreatProfile=<id>` は変更しません。特徴 index の定義も従来どおりです。`ThreatWidth` は Threat のある net にのみ使え、0<T<=L1、T は128の倍数です。
- 既存の FT hash・駒 FT・PSQT の後に、従来と同じ optional ThreatProfile の little-endian u32、続いて Threat 重みを置きます。重みは `[特徴 index][T 列]` の行優先、符号付き i8 の raw byte で、padding・bias はありません。その直後は既存の bucket ごとの FC hash・重みです。
- T 列を i16 accumulator に符号拡張して wrapping 加減算します。i16 の片側駒 FT は L1 列のままで、Threat の前半 T/2 列を駒 FT の `[0,T/2)`、後半を `[L1/2,L1/2+T/2)` に wrapping 加算します。他の列は駒 FT の値を使います。
- 加算後に従来と同じ clamp(0,127)、前半×後半、右7bitシフトの pairwise 変換を行います。Threat の量子化尺度は駒 FT と同じ整数単位で、追加の scale はありません。後段の量子化・FV_SCALE・PSQT 加算は変更しません。
- PSQT は従来の独立した駒由来 i32 accumulator です。Threat 重みの PSQT 項はありません。

部分幅の net は FT と後段の寸法、Threat 幅が一致する edition で読み込みます。
256列では 1536x15x64 の `threat256` を選びます。
512列では 1024x15x64 / 1536x15x32 / 1536x15x64 / 2304x15x64 の `threat512`、768列では
1024x15x64 / 1536x15x64 / 2304x15x64 の `threat768` を選びます。全幅の net は同じ寸法の `threat` edition を使います。
`edition-universal` は全幅の profile 0 に対応します。
edition 名は [ビルドガイド](../../../docs/build.md) を参照してください。

```bash
cargo run -q -p xtask -- build \
  --edition layerstacks-halfka_hm_merged-1536x15x64-threat256 \
  --features use-lazy-evaluate --profile production
```

PSQT 付き net 用の engine は、`cargo build` で edition と `nnue-psqt` を指定します。`xtask build` の追加 feature では `nnue-psqt` は edition の構成要素として拒否されます。

```bash
cargo build -p rshogi-usi --no-default-features --profile production \
  --features edition-layerstacks-halfka_hm_merged-1536x15x64-threat256,nnue-psqt,use-lazy-evaluate
```
