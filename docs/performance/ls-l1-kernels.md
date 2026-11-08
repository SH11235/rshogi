# LayerStacks L1 の AVX-512 カーネル

静的 LayerStacks の L1=1536 / 2304 / 3072、L1 出力=16 では、FT 出力変換と L1 積和を
AVX-512 で計算する。build の `target_feature` に `avx512f` / `avx512bw` /
`avx512vnni` がすべてある場合に有効になる。非 VNNI の AVX-512F/BW では、
通常 LayerStacks の 1536 に限り非 VNNI の融合経路を使う。それ以外の形状・ISA と
動的な universal 経路は従来の変換・密 L1 を使う。

## 変換と積和

1536 / 2304 と Threat 経路では、各視点の accumulator から 64 出力を生成し、register に保持したまま積和する。
自然順への permutation や中間 u8 バッファへの store は行わない。
変換式は `clamp(a,0,127) * clamp(b,0,127) >> 7`。

`packus(p0,p1)` の結果を `vpshufd` の `0x00/0x55/0xaa/0xff` で複製し、
非飽和の `vpdpbusd` で 16 本の独立したアキュムレータに加算する。
`dpbusd_register` は、オペランドを register に固定して spill と
重みロードの load-op 化（積和のメモリオペランドへの畳み込み）を避けるため、
積和の 1 命令を inline asm の `zmm_reg` オペランドで指定する。
最後に組と lane の和を取り、bias を一度加えて既存の後段へ渡す。
積和は 2^32 を法とする加算なので、分割と加算順序によらず一致する。
3072 の通常経路では、変換結果を自然順のバッファに置き、非ゼロの 4 byte 塊の位置を
u16 に圧縮する。対応する重みだけを読み、4 本の独立した `vpdpbusd` の積和に分ける。
4 byte がすべてゼロの塊だけを除外し、最後に bias を加えるため、密な計算と一致する。
Threat を含む場合は上記の融合経路を使う。

## 融合用の重みと編集

各 bucket は通常の L1 重みに加え、並べ替え済みコピーを L1×16 B 持つ
（1536 / 2304 / 3072 では 24 / 36 / 48 KiB、9 bucket では 216 / 324 / 432 KiB）。
`FusedWeights` は `reorder` だけが構築でき、
要素数 L1×16、64B アラインメント、重みの配置を不変条件とする。
通常ロードと prepacked ロードで読み込んだ層から直接コピーを構築する。
対象外の形状・ISA にはコピーを確保せず、評価中の確保もない。

各視点は L1/2 要素を出力し、64 要素ずつ 12 / 18 / 24 block に分ける。
後半の因子は accumulator の L1/2 先から読む。両視点を合わせた block 数は L1/64。

元の重み配置を `old[chunk][output][byte]` とすると、コピーは
`new[block][group][j][lane][dword][byte]` で、次の写像を使う。

```text
chunk  = 16 * block + 2 * lane + (j & 1) + 8 * (j >> 1)
output = 4 * group + dword
new[block][group][j][lane][dword][byte] = old[chunk][output][byte]
```

`LayerStackBucket` の L1 は `l1()` で読み、`edit_l1` の closure 内で編集する。
編集後は融合用コピーを再構築する。closure が unwind しても guard が
変更済みの L1 からコピーを再構築する。層からの構築には `from_layers` を使う。
自然順の変換 API、`evaluate_raw`、診断用 API は従来の密 L1 を使う。

## 検証

```sh
cargo test -j 4 -p rshogi-core --lib --no-default-features --features edition-layerstacks-halfka_hm_merged-2304x16x64-none,prepacked-nnue,diagnostics synthetic_fused_shapes
cargo test -j 4 -p rshogi-core --lib --no-default-features --features edition-layerstacks-halfka_hm_merged-2304x16x64-none,prepacked-nnue,diagnostics kernel_tests
cargo test -j 4 -p rshogi-core --lib --no-default-features --features edition-layerstacks-halfka_hm_merged-3072x16x64-none,prepacked-nnue,diagnostics packed_fused_shapes
```

非 ignore のテストで、合成重みの L1=1536 / 2304 / 3072、出力16 のネットワークを使い、全 9 bucket の
`evaluate_with_bucket` が先手番・後手番ともスカラー変換＋従来の密 L1 と一致することを確認する。
accumulator は直接入力し、このテストで使わない FT / PSQT / Threat 重みは確保しない。
カーネル単体では乱数重み、i16 全域と境界入力、i32 bias の wrap、L1 の編集・差し替え・
unwind、prepacked の copy-on-write を参照実装と照合する。

AVX-512 を持たない機械ではカーネルを実行せず、次の静的検査だけを行う。
明示的な `--target` により build script / proc macro は host 向けに構築する。

```sh
RUSTFLAGS="-C target-feature=+avx512f,+avx512bw,+avx512vnni" CARGO_TARGET_DIR=target/avx512chk cargo check -j 4 --target x86_64-unknown-linux-gnu -p rshogi-core -p rshogi-usi --tests --no-default-features --features edition-layerstacks-halfka_hm_merged-2304x16x64-none,prepacked-nnue,diagnostics
RUSTFLAGS="-C target-feature=+avx512f,+avx512bw,+avx512vnni" CARGO_TARGET_DIR=target/avx512chk cargo clippy -j 4 --target x86_64-unknown-linux-gnu -p rshogi-core -p rshogi-usi --tests --no-default-features --features edition-layerstacks-halfka_hm_merged-3072x16x64-none,prepacked-nnue,diagnostics -- -D warnings
```

カーネルの実行テストには対応 ISA を有効にした build が必要。

命令列はコンパイラと ISA に依存する。最終 production build の逆アセンブルで、
融合 loop の `vpdpbusd` が register オペランドを使い、loop 内に zmm の
store/reload（spill）がないことを確認する。debug assertion 付き build は
intrinsic 内の検査が残りうるため、性能評価には使用しない。
