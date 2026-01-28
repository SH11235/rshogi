# NNUE アーキテクチャ自動検出

## 概要

rshogi は NNUE ファイルのアーキテクチャ（FeatureSet, L1, L2, L3）を**ファイルサイズから一意に検出**する。
ヘッダーの description 文字列は活性化関数の検出にのみ使用し、L1/L2/L3 の判定には使用しない。

これにより、nnue-pytorch がハードコードした不正確なヘッダーを持つファイルでも正しく読み込める。

## 背景

### nnue-pytorch のハードコード問題

nodchip/nnue-pytorch の将棋用ブランチでは、`serialize.py` がアーキテクチャ文字列をハードコードしている:

```python
# serialize.py (shogi.* ブランチ全て)
description = b"Features=HalfKP(Friend)[125388->256x2],"
description += b"Network=AffineTransform[1<-256](ClippedReLU[256]..."
```

| ブランチ | 実際のアーキテクチャ | ヘッダーの記載 |
|---------|---------------------|---------------|
| shogi.2024-05-21.halfkp_768x2-8-96 | 768x2-8-96 | 256x2-256-256 |
| shogi.2025-07-28.halfkp_768x2-8-96 | 768x2-8-96 | 256x2-256-256 |

### bullet-shogi の場合（正しいヘッダー出力）

[bullet-shogi](https://github.com/SH11235/bullet-shogi/tree/shogi-support) では、
アーキテクチャ文字列を実際のパラメータから動的に生成するため、この問題は発生しない。

## 検出アルゴリズム

### 基本方針

**ファイルサイズからアーキテクチャを一意に検出する。**

アーキテクチャ（FeatureSet, L1, L2, L3）が決まれば、network_payload（純粋なネットワークデータサイズ）は一意に決まる。
逆に、network_payload からアーキテクチャを逆引きできる。

### network_payload の定義

```
network_payload = FT_bias + FT_weight + l1_bias + l1_weight + l2_bias + l2_weight + output_bias + output_weight
```

これはヘッダー (12 + arch_len) と hash (0 or 8) を除いた純粋なネットワークデータ。

### ファイル構造

```
ヘッダー付き + hash有り: file_size = 12 + arch_len + 8 + network_payload
ヘッダー付き + hash無し: file_size = 12 + arch_len + network_payload
```

### 検出フロー

```
1. file_size を取得
2. VERSION を読む（0x7AF32F16 or 0x7AF32F17）
3. arch_len を読む（offset 8-12）
4. base = file_size - 12 - arch_len を計算
5. 全サポートアーキテクチャに対して期待 network_payload と比較
   - base == expected_payload     → hash無し形式
   - base == expected_payload + 8 → hash有り形式
6. 一致したアーキテクチャで読み込み
7. 活性化関数はヘッダー文字列から検出（-SCReLU, -Pairwise など）
```

### 厳密性

- ファイルサイズは 1 バイト単位で正確
- 「±8B」ではなく「+0B または +8B」のみ許容
- 不一致の場合はエラー（未知のアーキテクチャ）

## サポートアーキテクチャ

### HalfKP

| L1 | L2 | L3 | network_payload |
|----|----|----|-----------------|
| 256 | 32 | 32 | 64,216,868 |
| 512 | 8 | 96 | 128,410,128 |
| 512 | 32 | 32 | 128,432,432 |
| 768 | 16 | 64 | 192,624,516 |
| 1024 | 8 | 32 | 256,814,288 |
| 1024 | 8 | 96 | 256,816,656 |

### HalfKA_hm

| L1 | L2 | L3 | network_payload |
|----|----|----|-----------------|
| 256 | 32 | 32 | 37,550,372 |
| 256 | 8 | 96 | 37,537,988 |
| 512 | 8 | 96 | 75,077,124 |
| 512 | 32 | 32 | 75,099,428 |
| 1024 | 8 | 96 | 150,150,660 |
| 1024 | 8 | 32 | 150,148,292 |

### HalfKA

| L1 | L2 | L3 | network_payload |
|----|----|----|-----------------|
| 256 | 32 | 32 | 70,934,692 |
| 512 | 8 | 96 | 141,846,868 |
| 1024 | 8 | 96 | 283,690,500 |

## 実装

### 関連ファイル

| ファイル | 役割 |
|---------|------|
| `crates/rshogi-core/src/nnue/spec.rs` | network_payload 計算、検出関数 |
| `crates/rshogi-core/src/nnue/network.rs` | 読み込みエントリーポイント |

### 主要関数

```rust
// network_payload を計算
pub const fn network_payload_halfkp(l1: usize, l2: usize, l3: usize) -> u64
pub const fn network_payload_halfka_hm(l1: usize, l2: usize, l3: usize) -> u64
pub const fn network_payload_halfka(l1: usize, l2: usize, l3: usize) -> u64

// ファイルサイズからアーキテクチャを検出
pub fn detect_architecture_from_size(
    file_size: u64,
    arch_len: usize,
    feature_set_hint: Option<FeatureSet>,
) -> Option<ArchDetectionResult>
```

## 新しいアーキテクチャの追加

新しい L1/L2/L3 の組み合わせをサポートする場合:

1. `halfkp/l{L1}.rs` または対応するモジュールに型エイリアスを追加
2. `spec.rs` の `KNOWN_PAYLOADS` テーブルにエントリを追加

```rust
const KNOWN_PAYLOADS: &[(FeatureSet, usize, usize, usize, u64)] = &[
    // 既存エントリ...
    (FeatureSet::HalfKP, 768, 8, 96, network_payload_halfkp(768, 8, 96)), // 新規
];
```

## 対応状況

| Feature Set | 自動検出 | 備考 |
|-------------|----------|------|
| HalfKP | ✓ あり | ファイルサイズで検出 |
| HalfKA_hm | ✓ あり | ファイルサイズで検出 |
| HalfKA | ✓ あり | ファイルサイズで検出 |
| LayerStacks | - | 固定アーキテクチャ（ヘッダーで判定） |

## FT hash について（参考情報）

以前のバージョンでは FT hash を使って L1 を検出していたが、現在は使用していない。
ファイルサイズで一意に決まるため、FT hash のチェックは不要。

ただし、FT hash の情報は以下の通り:

### HalfKP

```
FT hash = 0x5D69D5B8 ^ (L1 * 2)
```

| L1 | FT hash |
|----|---------|
| 256 | 0x5D69D7B8 |
| 512 | 0x5D69D1B8 |
| 768 | 0x5D69D3B8 |
| 1024 | 0x5D69DDB8 |

### HalfKA_hm / HalfKA

```
FT hash = 0x5F134CB8 ^ (L1 * 2)
```

| L1 | FT hash |
|----|---------|
| 256 | 0x5F134EB8 |
| 512 | 0x5F1348B8 |
| 1024 | 0x5F1344B8 |

**Note:** bullet-shogi 生成ファイルは FT hash = 0 だが、ファイルサイズで検出可能。

## 参考

- [AobaNNUE](https://github.com/yssaya/AobaNNUE) - HalfKP 768x2-16-64 の出典
- [bullet-shogi](https://github.com/SH11235/bullet-shogi) - 正しいヘッダーを出力する学習器
