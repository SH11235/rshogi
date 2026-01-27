# NNUE アーキテクチャ自動検出

## 概要

rshogi は NNUE ファイルのアーキテクチャ（L1/L2/L3）を自動検出する。
nnue-pytorch でエクスポートされたファイルはヘッダーのアーキテクチャ文字列がハードコードされており、
実際のモデル構造と一致しない場合がある。この問題を FT hash とファイルサイズを使って解決する。

## 背景

### nnue-pytorch のハードコード問題

nodchip/nnue-pytorch の将棋用ブランチでは、`serialize.py` がアーキテクチャ文字列をハードコードしている。
これは 2021年〜2025年 の全ブランチで一貫している:

```python
# serialize.py (shogi.* ブランチ全て)
description = b"Features=HalfKP(Friend)[125388->256x2],"
description += b"Network=AffineTransform[1<-256](ClippedReLU[256]..."
```

| ブランチ | 実際のアーキテクチャ | ヘッダーの記載 |
|---------|---------------------|---------------|
| shogi.2024-05-21.halfkp_768x2-8-96 | 768x2-8-96 | 256x2-256-256 |
| shogi.2025-07-28.halfkp_768x2-8-96 | 768x2-8-96 | 256x2-256-256 |

※ master ブランチ（チェス用）では `--description` オプションで指定可能だが、将棋開発では使われていない。

### bullet-shogi の場合（正しいヘッダー出力）

[bullet-shogi](https://github.com/SH11235/bullet-shogi/tree/shogi-support) では、
アーキテクチャ文字列を実際のパラメータから動的に生成するため、この問題は発生しない:

```rust
// examples/shogi_simple.rs (shogi-support ブランチ)
let arch_str = format!(
    "Features={}[{}->{}]{},fv_scale={},l1_input={},l2={},l3={},qa={},qb={},scale={},pairwise={}",
    feature_name, input_size, l0_suffix, activation_suffix,
    fv_scale, l1_input_dim, l2_size, l3_size, qa, qb, args.scale, pairwise_enabled
);
```

bullet-shogi でエクスポートしたファイルは、ヘッダーに正しい L1/L2/L3 が記載されるため、
rshogi は通常のヘッダーパースで正しく読み込める（FT hash による検出は不要）。

### 問題の例

`AobaNNUE_HalfKP_768x2_16_64_FV_SCALE_40.bin` を読み込もうとすると:

```
Unsupported HalfKP L1=256 architecture: L2=256, L3=256, activation=CReLU
```

## 解決策

### FT hash による L1 検出

FT hash は L1 から一意に計算可能:

```
FT hash = HALFKP_HASH ^ (L1 * 2)
HALFKP_HASH = 0x5D69D5B8  // HalfKP(Friend) のベースハッシュ
```

| L1 | 計算式 | FT hash |
|----|--------|---------|
| 256 | 0x5D69D5B8 ^ 512 | 0x5D69D7B8 |
| 512 | 0x5D69D5B8 ^ 1024 | 0x5D69D1B8 |
| 768 | 0x5D69D5B8 ^ 1536 | 0x5D69D3B8 |
| 1024 | 0x5D69D5B8 ^ 2048 | 0x5D69DDB8 |

### ファイルサイズによる L2/L3 検出

L1 が判明すれば、ファイルサイズから L2/L3 を特定できる。

#### ファイルサイズ計算式

```
header = 12 + arch_len  // version(4) + hash(4) + arch_len(4) + arch_str
ft_hash = 4
ft_bias = L1 * 2
ft_weight = 125388 * L1 * 2
network_hash = 4
l1_bias = L2 * 4
l1_weight = pad32(L1 * 2) * L2
l2_bias = L3 * 4
l2_weight = pad32(L2) * L3
output_bias = 4
output_weight = L3

total = sum(above)
```

※ `pad32(n) = ceil(n / 32) * 32`

#### 既知の組み合わせ

| L1 | L2 | L3 | ファイルサイズ (arch_len=184) |
|----|----|----|------------------------------|
| 256 | 32 | 32 | 64,280,784 |
| 512 | 8 | 96 | 128,556,784 |
| 512 | 32 | 32 | 128,558,576 |
| 768 | 16 | 64 | 192,624,720 |
| 1024 | 8 | 32 | 256,902,768 |
| 1024 | 8 | 96 | 256,904,816 |

## 検出フロー

```
1. ヘッダーをパース
   - parse_architecture() で L1/L2/L3 を取得

2. ヘッダーが不正確か判定
   - L2 == 0 または L3 == 0
   - L2 == 256 かつ L3 == 256 (nnue-pytorch のハードコード値)

3. 不正確な場合、FT hash から L1 を検出
   - offset = 12 + arch_len
   - 既知の L1 値 (256, 512, 768, 1024) で期待 FT hash を計算
   - 一致する L1 を採用

4. ファイルサイズから L2/L3 を検出
   - 検出した L1 に対応する既知の L2/L3 候補を列挙
   - 各候補でファイルサイズを計算し、実際のサイズと照合
   - 一致すれば採用、一致しなければデフォルト値を使用

5. 検出したアーキテクチャでネットワークを読み込み
```

## 実装

### 関連ファイル

| ファイル | 役割 |
|---------|------|
| `crates/rshogi-core/src/nnue/spec.rs` | L1/L2/L3 検出関数 |
| `crates/rshogi-core/src/nnue/network.rs` | 読み込みロジック |

### 主要関数

```rust
// spec.rs

/// FT hash から L1 を検出
pub fn detect_halfkp_l1_from_ft_hash(ft_hash: u32) -> Option<usize>

/// L1 に対応するデフォルトの L2/L3 を取得
pub fn default_halfkp_l2_l3(l1: usize) -> (usize, usize)

/// ファイルサイズから L2/L3 を検出
pub fn detect_halfkp_l2_l3_from_size(
    l1: usize,
    file_size: u64,
    arch_len: usize,
) -> Option<(usize, usize)>

/// 期待されるファイルサイズを計算
pub fn expected_halfkp_file_size(
    l1: usize,
    l2: usize,
    l3: usize,
    arch_len: usize,
) -> u64
```

### network.rs での使用

```rust
FeatureSet::HalfKP => {
    let (l1, l2, l3) = if parsed.l2 == 0
        || parsed.l3 == 0
        || (parsed.l2 == 256 && parsed.l3 == 256)
    {
        // FT hash から L1 を検出
        let ft_hash = /* read from file */;
        let detected_l1 = detect_halfkp_l1_from_ft_hash(ft_hash)?;

        // ファイルサイズから L2/L3 を検出
        let file_size = /* get file size */;
        let (l2, l3) = detect_halfkp_l2_l3_from_size(detected_l1, file_size, arch_len)
            .unwrap_or_else(|| default_halfkp_l2_l3(detected_l1));

        (detected_l1, l2, l3)
    } else {
        (parsed.l1, parsed.l2, parsed.l3)
    };

    HalfKPNetwork::read(reader, l1, l2, l3, activation)?
}
```

## 新しいアーキテクチャの追加

新しい L1/L2/L3 の組み合わせをサポートする場合:

1. `halfkp/l{L1}.rs` に型エイリアスを追加（または新規作成）
2. `spec.rs` の `detect_halfkp_l2_l3_from_size()` に候補を追加

```rust
let candidates: &[(usize, usize)] = match l1 {
    256 => &[(32, 32)],
    512 => &[(8, 96), (32, 32)],
    768 => &[(16, 64)],           // ← 新規
    1024 => &[(8, 32), (8, 96)],
    _ => return None,
};
```

## 参考

- [AobaNNUE](https://github.com/yssaya/AobaNNUE) - HalfKP 768x2-16-64 の出典
