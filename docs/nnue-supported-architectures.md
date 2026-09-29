# サポートされている NNUE アーキテクチャ

モデルの形式・形状に合うエンジンを使ってください。複数の形式を使う場合は既定の
`edition-universal`、固定 edition を使う場合はモデルと一致する構成を選びます。
ビルド方法と edition 一覧は [ビルドガイド](./build.md) を参照してください。

モデルは `EvalFile` で指定し、`FV_SCALE` は配布元の推奨値に合わせます。
LayerStacks では、下記の振り分け方式も `isready` の前に設定してください。

## HalfKP / HalfKA 系

以下は対応する L1 と L2×L3 の組み合わせです。
モデルの配布元が示す特徴量・形状・活性化関数を確認してください。

| L1 | HalfKP | HalfKA_hm | HalfKA |
|----|--------|-----------|--------|
| 256 | 32×32 | 32×32 | 32×32 |
| 512 | 8×64、8×96、32×32 | 8×64、8×96、32×32 | 8×64、8×96、32×32 |
| 768 | 16×64 | 16×64 | 16×64 |
| 1024 | 8×32、8×64 | 8×32、8×64、8×96 | 8×32、8×64、8×96 |

HalfKP は水匠・tanuki・AobaNNUE などで使われる classic NNUE 形式です。
HalfKA_hm は Half-Mirror、HalfKA は Non-mirror 形式です。
bullet-shogi や nnue-pytorch のモデルも、生成ソフト名だけでなく形式と形状を確認してください。

活性化関数は CReLU、SCReLU、PairwiseCReLU に対応します。
読み込み時に検出されるため、使用するモデルに合わせてください。
検出の詳細は [アーキテクチャ自動検出](./nnue-architecture-detection.md) を参照してください。

## LayerStacks

`edition-universal` はモデルから形状を読み取ります。
固定構成の例は以下です。対応する edition は [ビルドガイド](./build.md) で確認してください。

| L1 | LayerStack の出力サイズ（L1×L2） |
|----|--------------------------------|
| 512 | 16×32 |
| 768 | 8×32、16×32 |
| 1024 | 16×32 |
| 1536 | 16×32、32×32 |

### 振り分け方式の設定

学習時と同じ `LS_BUCKET_MODE` を指定します。

| モデルの方式 | `LS_BUCKET_MODE` | 追加設定 |
|--------------|-----------------|----------|
| KingRank9 / SFNN k3k3 | `kingrank9` | なし。9バケットのモデルを使用 |
| tatara の進行度方式 | `progresskpabs` | `LS_PROGRESS_BUCKETS` と `LS_PROGRESS_COEFF` |
| YaneuraOu / BulletOu SFNN の進行度方式 | `progresskpabsq16` | `LS_PROGRESS_BUCKETS` と `LS_PROGRESS_COEFF` |

- `LS_PROGRESS_BUCKETS` は学習時のバケット数を指定します。範囲は1〜16で、モデルの格納数以下です。
- `LS_PROGRESS_COEFF` には、そのモデルの学習に使った進行度係数ファイルを指定します。バケット数1では不要です。
- `kingrank9` では `LS_PROGRESS_BUCKETS` を0（未指定）、`LS_PROGRESS_COEFF` を未指定にします。
- tatara と YaneuraOu 系の進行度方式は計算結果が異なるため、同じ係数ファイルでも置き換えて使わないでください。
- YaneuraOu と BulletOu の進行度方式が一致するバケット数は2・4・8・16です。k3k3 と進行度を組み合わせた方式は未対応です。

SFNN の振り分け方式への対応と、ファイル形式の互換性は別です。
YaneuraOu SFNN 形式のファイルは、rshogi で読み込める形式への変換が必要です。
tools の native 評価経路には Q16 未対応のものがあります。
対象と利用方法は [tools の LayerStacks routing 対応](../crates/tools/docs/nnue-routing.md) を参照してください。
