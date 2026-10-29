# サポートされている NNUE アーキテクチャ

モデルの形式・形状に合うエンジンを使ってください。複数の形式を使う場合は既定の
`edition-universal`、固定 edition を使う場合はモデルと一致する構成を選びます。
ビルド方法と edition 一覧は [ビルドガイド](./build.md) を参照してください。

モデルは `EvalFile` で指定し、`FV_SCALE` は配布元の推奨値に合わせます。
LayerStacks では、下記の振り分け方式も `isready` の前に設定してください。

## edition の選択と読込条件

ここでの「対応」は reader と評価経路が実装されていることを表します。
すべての学習済みモデル・ISA の組合せで推論一致を保証する意味ではありません。
配布モデルの特徴量、量子化形式、形状、拡張、routing を一緒に確認してください。

形状の `L1×L2×L3` は rshogi の表記で、FT 出力幅、最初の全結合層出力幅、
次の全結合層出力幅を表します。tatara の `ft_out / l1_out / l2_out` に対応し、
最終出力は1です。例えば tatara の `l2_out=64` は本表の `L3=64` です。

| 軸 | `edition-universal` | 固定 edition / family edition |
|----|---------------------|-------------------------------|
| HalfKX の FT | HalfKP、HalfKaSplit、HalfKaMerged、HalfKaHmSplit、HalfKaHmMerged | ビルドに含めた FT のみ |
| LayerStacks の FT | 上記5種、HalfKaHmMerged の EffectBucket | FT と EffectBucket 設定をビルドに含める |
| HalfKX の形状 | 各幅1〜16384。PairwiseCReLU の L1 は偶数。ヘッダー・payload の整合が必要 | 下記の静的構成とビルドの組合せ |
| LayerStacks の形状 | L1 は正の偶数、L2 は2以上、L3 は1以上。各幅16384以下 | 下記の10構成からビルド時に選択 |
| HalfKX の活性化 | CReLU / SCReLU / PairwiseCReLU | ビルドに含めた活性化のみ |
| LayerStacks の活性化 | Product Pooling、SCReLU と CReLU の混在、skip を持つ専用構造 | 同じ専用構造。任意の活性化グラフは非対応 |
| PSQT | LayerStacks のヘッダーから読み込む | `nnue-psqt` を含む構成 |
| ThreatProfile | profile 0 のみ | モデルと同じ profile を含む構成 |
| EffectBucket | 2×2 / 3×3、kingfixed / kingbucketed をヘッダーから選択 | `nnue-effect-bucket` と対応する設定が必要 |
| factorized | 学習用の未統合重みは非対応。coalesced な推論形式へ書き出す | 同左 |

寸法の上限はメモリ使用量の保証ではありません。FT の入力次元と出力幅、Threat、
EffectBucket などに応じて重みが大きくなり、積の overflow や payload の不足など、
読込時に検出できる不整合はエラーになります。メモリ不足は実行経路によってプロセス終了になるため、十分なメモリを用意してください。固定 edition は必要な FT・サイズ・拡張が揃っていても、
異なる重み配置や量子化形式を変換するものではありません。
FT・サイズ・拡張のすべての組合せに名前付き preset があるわけではありません。
[ビルドガイド](./build.md) で preset を確認し、それ以外は対応する atomic feature を
組み合わせてください。family edition にも拡張を追加する必要がある場合があります。

### 形式と量子化

- HalfKX は対応する classic NNUE ヘッダーと整数 payload を使います。
  FT の重み・bias は i16、全結合層の重みは i8、bias は i32 です。
- LayerStacks は `0x7af32f20`（格納9バケット）または `0x7af32f21`
  （格納数を明示、1〜16バケット）です。通常の FT は LEB128 圧縮された i16、
  全結合層は i8 重みと i32 bias、PSQT は i32、Threat 重みは i8 です。
- tatara の LayerStacks 推論用量子化は QA=127、QB=64 です。
  float checkpoint は推論用の量子化形式へ書き出してください。別の QA/QB の整数モデルは読み込めても不一致を検出できず、誤った評価値になる可能性があります。
  `FV_SCALE` は最終評価値の換算であり、重みの量子化形式を変換しません。

### ThreatProfile と利用できないモデル

非ゼロの ThreatProfile を universal で読むと
`Threat dimensions mismatch: model=…, runtime=…`、または次元数が同じ profile 4 では
`runtime threat profiles other than profile 0 are not yet supported` という読込エラーになります。
profile を削除して読み込ませず、対応する固定構成を使ってください。

| profile | 固定構成に追加する feature |
|---------|---------------------------|
| 0（全ペア） | `nnue-threat` |
| 1 | `nnue-threat,threat-profile-same-class` |
| 2 | `nnue-threat,threat-profile-same-class-major-pawn` |
| 3 | `nnue-threat,threat-profile-step-attacker` |
| 4 | `nnue-threat,threat-profile-full-symdedup` |
| 10 | `nnue-threat,threat-profile-cross-side` |

固定構成は FT・寸法も一致させます。profile feature はモデルと一致するものを1つだけ
選びます。profile 4 と0は次元数が同じでも特徴の意味が異なるため、互換ではありません。
未統合の `Factorizer`、未知の FT、サイズ不一致、範囲外のバケット数は読込エラーとして
扱い、推論用の再出力または適合する edition を選び直してください。

### ISA と検証範囲

モデルファイルの形式は ISA 共通ですが、バイナリは実行環境に合わせてください。

| 実行環境 | load / evaluate の条件 |
|----------|------------------------|
| x86-64 scalar / SSSE3 / AVX2 等 | ビルドが要求する命令セットを CPU が備えること。モデルの受入条件は上表と同じ |
| AArch64 | 対応 target 向けにビルドすること。モデルの受入条件は上表と同じ |
| WASM scalar / SIMD128 | scalar または SIMD128 対応環境用にビルドすること。universal / fixed ともモデルの条件は上表と同じ |

SIMD の有無で、未対応の ThreatProfile やモデル形式が対応になることはありません。
各 target の作成方法は [ビルドガイド](./build.md) を参照してください。
この対応表は全 ISA に対する実モデルの一致試験一覧ではありません。

tatara との照合では、参照評価器 `eval_sfen` が扱う FT と拡張にも注意してください。
HalfKaHmMerged を基準にする評価器で他の FT の一致を証明することはできません。
PSQT を加算しない参照経路では PSQT 付きモデルを比較対象から外します。
進行度方式では格納バケット数と実際の routing 数、係数、`FV_SCALE` を揃え、
評価値の単位も一致させてください。rshogi [`eval_sfens`](../crates/tools/docs/eval_sfens.md) の `score_cp` は歩=100への
換算値なので、tatara の出力との比較には `score` を使います。

## HalfKP / HalfKA 系

以下は固定構成の L1 と L2×L3 の組み合わせです。universal の幅の制約は上表を参照してください。
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
固定構成のサイズ一覧は以下です。対応する edition は [ビルドガイド](./build.md) で確認してください。

| L1 | 全結合層の出力サイズ（L2×L3） |
|----|--------------------------------|
| 512 | 16×32 |
| 768 | 8×32、16×32 |
| 1024 | 16×32 |
| 1536 | 16×32、32×32、16×64 |
| 2304 | 16×64 |
| 3072 | 16×32、16×64 |

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
tools の native 評価経路も 3 方式に対応します。
対象と利用方法は [tools の LayerStacks routing 対応](../crates/tools/docs/nnue-routing.md) を参照してください。
