# NNUE アーキテクチャ自動検出

モデルの受入条件は [NNUE 対応表](./nnue-supported-architectures.md)、
edition の選択は [ビルドガイド](./build.md) を参照してください。
読み込みはファイルのバージョン、ヘッダー、payload のサイズを組み合わせて判定します。

## edition 別の検出規則

| モデル | universal edition | 固定・family edition |
|---|---|---|
| HalfKX（HalfKP / HalfKA 系） | 既知の payload サイズに一致すれば、その寸法を優先。未一致時はヘッダーの寸法を検証して使う | 既知の payload サイズに一致する構成を選び、ビルドに含まれない構成は拒否 |
| LayerStacks | ヘッダーから FT・寸法・拡張・格納バケット数を読み、実行時に構成 | ヘッダーの構成とビルドに含まれる FT・寸法・拡張を照合 |

HalfKX の特徴集合と活性化関数もヘッダーから判定します。
Auto では FT 入力寸法を検査し、学習用の未統合な `Factorizer` 重みは受け付けません。
既知のファイルサイズを優先することで、層の寸法表記が不正確な旧 nnue-pytorch モデルにも対応します。
ただし、ヘッダーの誤りをすべて自動修正するものではありません。

LayerStacks の FT は LEB128 圧縮されるため、ファイルサイズから層の寸法を逆引きしません。
`0x7af32f20` は格納9バケット、`0x7af32f21` はヘッダーに格納数を持つ形式です。
バケットの振り分け方式はモデルの形状だけでは決まらないため、学習時と同じ routing 設定も必要です。

### HalfKX の既知サイズとの照合

ヘッダーの長さを除いたサイズを、特徴集合ごとの既知の `L1 / L2 / L3` と照合します。
payload は次の合計です。

```text
network_payload = FT_bias + FT_weight
                + l1_bias + l1_weight + l2_bias + l2_weight
                + output_bias + output_weight
```

各サイズはバイト単位です。ヘッダーは `12 + arch_len` バイト、FT とネットワークの hash は計8バイトです。
既知サイズの照合では、hash を含まない payload と、hash を加えた payload + 8 のみを一致と扱います。
任意の誤差を許容する判定ではありません。

universal の未知サイズはヘッダー寸法から必要バイト数を求め、payload の不足や不正な末尾を拒否します。
固定・family edition では既知サイズに一致しない場合、`Unknown architecture` と候補との差を返します。
対応する寸法の一覧は [NNUE 対応表](./nnue-supported-architectures.md#halfkp--halfka-系) にまとめています。

## 特徴集合の明示補正

USI の `NNUE_ARCHITECTURE` で特徴集合を明示すると、universal と固定 edition の両方で
指定した特徴集合を使います。誤記された feature 名や FT 入力寸法を補正できますが、
payload の形状・長さは選択した特徴集合に適合する必要があります。
活性化、層の寸法、その他の metadata を任意に置き換える指定ではありません。

通常は `Auto` を使ってください。別レイアウトや途中で切れたファイルを override で読ませることはできません。
LayerStacks 専用の `0x7af32f21` は HalfKX として読み込めません。

## 評価値の scale

HalfKP の `fv_scale` metadata は1〜128の整数のみを採用します。
0、負数、128超、不正表記、metadata なしの場合は既定24を使い、これだけでは読込エラーにしません。

USI の `FV_SCALE=0` はモデルの metadata または既定値を使います。
正の明示値は metadata より優先します。モデル metadata の許容範囲と USI override の契約は別です。
`FV_SCALE` は評価値の換算であり、重みの量子化形式は変換しません。

## 読み込めない場合

1. モデルの特徴集合・寸法・活性化・拡張に合う edition か確認します。
2. ヘッダーが誤記されている場合は、配布元が示す特徴集合を確認してから `NNUE_ARCHITECTURE` を指定します。
3. `Unknown architecture` や payload 不足の場合は、形式の違い・転送中の欠損を確認します。
4. LayerStacks の routing エラーは [対応表の設定手順](./nnue-supported-architectures.md#振り分け方式の設定) に従います。

## 実装の参照先

- `crates/rshogi-core/src/nnue/network.rs`: 読み込みの入口と edition ごとの振り分け
- `crates/rshogi-core/src/nnue/spec.rs`: ヘッダー解析、`network_payload_halfkp` /
  `network_payload_halfka_hm` / `network_payload_halfka` によるサイズ計算、
  `detect_architecture_from_size` による既知サイズとの照合
- `crates/rshogi-core/src/nnue/dynamic_halfkx.rs`: universal の HalfKX reader
- `crates/rshogi-core/src/nnue/dynamic_layer_stacks.rs`: universal の LayerStacks reader

## 参考

- [AobaNNUE](https://github.com/yssaya/AobaNNUE): HalfKP 768×2-16-64 の配布元
- [bullet-shogi](https://github.com/SH11235/bullet-shogi): NNUE 学習器
