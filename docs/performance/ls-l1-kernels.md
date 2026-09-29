# LayerStacks L1 の融合 VNNI カーネル

静的 LayerStacks の L1=1536、L1 出力=16 では、FT 出力変換と L1 積和を
融合して計算する。build の `target_feature` に `avx512f` / `avx512bw` /
`avx512vnni` がすべてある場合に有効になる。それ以外の形状・ISA と動的な
universal 経路は従来の変換・密 L1 を使う。

## 採用結果

最終版と main `9de40f85`（評価器 PR #1140 merge 後）を比較した
ETW search-only（5 局面 × 5 秒 × ABBA）の結果は以下のとおり。

| 関数配置 | NPS | cycles/node |
| --- | ---: | ---: |
| 既定 | +3.93% | −3.4% |
| `-align-all-functions=6` | +4.03% | −3.4% |
| `-align-all-functions=5` | +1.39% | −0.8% |

固定 depth 1〜18 × 5 局面で nodes / score / PV / bestmove が base と一致した。
参考として、最終版より前の同一バイナリ内での従来経路との比較は NPS +2.26% だった。
採用の根拠は上表の最終版の結果であり、CPU・コンパイラ・関数配置で改善率は変わる。

## 変換と積和

各視点の accumulator から 64 出力を生成し、register に保持したまま積和する。
自然順への permutation や中間 u8 バッファへの store は行わない。
変換式は `clamp(a,0,127) * clamp(b,0,127) >> 7`。

`packus(p0,p1)` の結果を `vpshufd` の `0x00/0x55/0xaa/0xff` で複製し、
非飽和の `vpdpbusd` で 16 本の独立したアキュムレータに加算する。
`dpbusd_register` は、オペランドを register に固定して spill と
重みロードの load-op 化（積和のメモリオペランドへの畳み込み）を避けるため、
積和の 1 命令を inline asm の `zmm_reg` オペランドで指定する。
上表はこの inline asm を含む融合カーネル全体の結果であり、asm 単独の改善率ではない。
最後に組と lane の和を取り、bias を一度加えて既存の後段へ渡す。
積和は 2^32 を法とする加算なので、分割と加算順序によらず一致する。

## 融合用の重みと編集

各 bucket は通常の L1 重みに加え、並べ替え済みコピーを 24 KiB 持つ
（9 bucket では 216 KiB）。`FusedWeights` は `reorder` だけが構築でき、
要素数 1536×16、64B アラインメント、重みの配置を不変条件とする。
通常ロードと prepacked ロードで読み込んだ層から直接コピーを構築する。
対象外の形状・ISA にはコピーを確保せず、評価中の確保もない。

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
cargo build -p rshogi-usi --no-default-features --features edition-layerstacks-halfka_hm_merged-1536x16x32-none
cargo test -p rshogi-core --no-default-features --features edition-layerstacks-halfka_hm_merged-1536x16x32-none
cargo test -p rshogi-core --no-default-features --features edition-layerstacks-halfka_hm_merged-1536x16x32-none,prepacked-nnue,diagnostics
```

非 ignore のテストで、合成重みの 1536×16 ネットワークを使い、全 9 bucket の
`evaluate_with_bucket` が先手番・後手番ともスカラー変換＋従来の密 L1 と一致することを確認する。
accumulator は直接入力し、このテストで使わない FT / PSQT / Threat 重みは確保しない。
カーネル単体では乱数重み、i16 全域と境界入力、i32 bias の wrap、L1 の編集・差し替え・
unwind、prepacked の copy-on-write を参照実装と照合する。

命令列はコンパイラと ISA に依存する。最終 production build の逆アセンブルで、
融合 loop の `vpdpbusd` が register オペランドを使い、loop 内に zmm の
store/reload（spill）がないことを確認する。debug assertion 付き build は
intrinsic 内の検査が残りうるため、性能評価には使用しない。
