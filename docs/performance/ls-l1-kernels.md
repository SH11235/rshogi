# LayerStacks L1 カーネルの screening

隠し USI combo `LsL1Kernel` で、同じバイナリ内の評価カーネルを比較できる。
`usi` の option 一覧には表示しない。既定値は `legacy`。

```text
setoption name LsL1Kernel value legacy
setoption name LsL1Kernel value xf64
setoption name LsL1Kernel value fused
```

| 値 | FT 出力変換 | L1 積和 |
|---|---|---|
| `legacy` | 既存の変換 | 既存の密 kernel |
| `xf64` | 64 出力ごとに packus と qword permutation、64B store | 既存の密 kernel |
| `fused` | 64 出力を register に保持、自然順へ戻さない | dword の lane 内複製と非飽和 VNNI |

対象は静的 LayerStacks 経路の L1=1536、L1 出力=16 で、build の
`target_feature` に `avx512f` / `avx512bw` / `avx512vnni` がある場合。
それ以外の形状・ISA、および動的な universal 経路は従来の評価を使う。
未知の option 値は警告して無視する。設定値は static Atomic に保存し、対象の
`evaluate_with_bucket` 入口で一度だけ読む。`evaluate_raw`、診断用 API、
自然順の変換 API はこの設定に依存しない。

## 融合用の重みと編集

各 bucket は通常の L1 重みに加え、64B 境界の並べ替え済みコピーを 24 KiB 持つ
（9 bucket では 216 KiB）。通常ロードと prepacked ロードで同じコピーを構築する。
対象外の形状・ISA にはこの配列を確保しない。評価中の確保はない。

元の重み配置を `old[chunk][output][byte]` とすると、コピーは
`new[block][group][j][lane][dword][byte]` で、次の写像を使う。

```text
chunk  = 16 * block + 2 * lane + (j & 1) + 8 * (j >> 1)
output = 4 * group + dword
new[block][group][j][lane][dword][byte] = old[chunk][output][byte]
```

各ブロックで `packus(p0,p1)` の結果を `vpshufd` の
`0x00/0x55/0xaa/0xff` で複製し、16 本の独立したアキュムレータに加算する。
最後に組と lane の和を取り、bias を一度加えて既存の後段へ渡す。
変換式は `clamp(a,0,127) * clamp(b,0,127) >> 7` のまま。
積和は 2^32 を法とする非飽和加算なので、分割と加算順序によらず一致する。

`LayerStackBucket` の L1 は `l1()` で読み、`edit_l1` の closure 内で編集する。
編集後は融合用コピーも再構築する。closure が unwind した場合はコピーを無効にし、
以後の fused 指定を legacy へフォールバックさせる。層から構築する場合は
`from_layers` を使う。SPSA net_delta の現在の対象には L1 重みは含まれない。

## 確認手順

```sh
cargo build -p rshogi-usi --no-default-features --features edition-layerstacks-halfka_hm_merged-1536x16x32-none
cargo test -p rshogi-core --no-default-features --features edition-layerstacks-halfka_hm_merged-1536x16x32-none
cargo test -p rshogi-core --no-default-features --features edition-layerstacks-halfka_hm_merged-1536x16x32-none,prepacked-nnue,diagnostics
NNUE_TEST_FILE="$SHOGI_DATA/nnue/<model>.bin" cargo test -p rshogi-core --no-default-features --features edition-layerstacks-halfka_hm_merged-1536x16x32-none test_load_layer_stacks_file -- --ignored --nocapture
```

単体テストは全 9 bucket の乱数重み、i16 全域と境界入力、i32 bias の wrap、
L1 の編集・差し替え、prepacked の copy-on-write を確認する。
実 net テストは全 bucket の L1 出力と静的評価値を比較する。

速度の採否には別途、競合のない環境で同一バイナリを使った測定が必要。
命令列はコンパイラと ISA に依存するので、最終 build の逆アセンブルで
融合 loop の spill と変換命令を確認する。debug assertion 付きの build は
intrinsic 内の検査が残りうるため、性能評価には使用しない。
