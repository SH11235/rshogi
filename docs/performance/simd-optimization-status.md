# SIMD 最適化状況とポリシー

## 1. ファイル別 SIMD 対応一覧

### NNUE 推論パス

| ファイル | 関数/処理 | AVX512 | AVX2 | SSSE3 | SSE2 | WASM | スカラー |
|---|---|:---:|:---:|:---:|:---:|:---:|:---:|
| `layers.rs` | `AffineTransform::propagate` | VNNI/BW | ✓ | ✓ | ✓ | ✓ | ✓ |
| `layer_stacks.rs` | `sqr_clipped_relu_transform` | BW | ✓ | — | ✓ | ✓ | ✓ |
| `layer_stacks.rs` | `LayerStackBucket::propagate` 内 activation | — | — | — | — | — | ✓ |
| `feature_transformer.rs` | `add_weights`/`sub_weights` | BW | ✓ | — | ✓ | ✓ | ✓ |
| `feature_transformer_layer_stacks.rs` | `add_weights`/`sub_weights` | BW | ✓ | — | ✓ | ✓ | ✓ |
| `activation.rs` | `crelu_i16_to_u8` | BW | ✓ | — | ✓ | ✓ | ✓ |
| `activation.rs` | `crelu_i32_to_u8` | F | ✓ | — | ✓ (SSE4.1 + SSE2) | — | ✓ |
| `activation.rs` | `pairwise_crelu_i16_to_u8` | F | ✓ | — | ✓ (SSE4.1 + SSE2) | ✓ | ✓ |
| `activation.rs` | `pairwise_crelu_i32_to_u8` | F | ✓ | — | ✓ (SSE4.1 + SSE2) | ✓ | ✓ |
| `activation.rs` | `screlu_i16_to_u8` | F | ✓ | — | ✓ | ✓ | ✓ |

### Bitboard

| ファイル | 関数/処理 | AVX2 | SSSE3 | SSE2 | スカラー |
|---|---|:---:|:---:|:---:|:---:|
| `bitboard256.rs` | 256bit 盤面演算 | ✓ | — | — | ✓ |
| `core.rs` | `byte_reverse` | — | ✓ | ✓ | ✓ |

---

## 2. SSE2 / SSSE3 / SSE4.1 の使い分け

SIMD ティアの選択は intrinsic の可用性で決まる。任意に選んでいるわけではない。

### SSSE3 が必要な場合: `_mm_maddubs_epi16`

`layers.rs` の `AffineTransform::propagate` (SSSE3パス) は u8 × i8 の内積に
`_mm_maddubs_epi16` を使う。この intrinsic は SSSE3 以降でしか利用できない。

```
SSSE3: _mm_maddubs_epi16(u8_input, i8_weight) → i16 result  // 1命令

SSE2:  unpack u8→i16 + sign-extend i8→i16 + mullo + madd    // 6命令
```

SSE2 フォールバックは同じ結果を得るために命令数が約6倍になる。

### SSE4.1 が必要な場合: `_mm_packus_epi32`

`activation.rs` の pairwise CReLU 系は i32→u8 のパックに `_mm_packus_epi32`
（SSE4.1）を使う。SSE2 にはこの命令がない。

### SSE2 で十分な場合

`layer_stacks.rs` の `sqr_clipped_relu_transform` や `feature_transformer*.rs` の
`add_weights`/`sub_weights` は `min`/`max`/`add`/`sub`/`packus_epi16` のみで、
全て SSE2 の範囲。SSSE3 パスは不要。

---

## 3. LayerStacks バケット内 activation のスカラー実装について

`LayerStackBucket::propagate` 内の以下2箇所はスカラーのみ:

### L1→L2 activation (layer_stacks.rs:88-96)

```rust
// 15要素の SqrClippedReLU + ClippedReLU → 30 u8
for (i, &val) in l1_out.iter().enumerate().take(NNUE_PYTORCH_L2) {
    let sqr = ((val as i64 * val as i64) >> 19).clamp(0, 127) as u8;
    let clamped = (val >> 6).clamp(0, 127) as u8;
    l2_input.0[i] = sqr;
    l2_input.0[NNUE_PYTORCH_L2 + i] = clamped;
}
```

### L2→Output activation (layer_stacks.rs:104-106)

```rust
// 32要素の ClippedReLU
for (out, &val) in l2_relu.0.iter_mut().zip(l2_out.iter()) {
    *out = (val >> 6).clamp(0, 127) as u8;
}
```

### SIMD 化しない理由

- **要素数が極めて小さい**: 15要素、32要素
- **ボトルネックではない**: 直前の `AffineTransform::propagate`（1536→16 の行列積）が
  支配的。activation は行列積に対して数%程度の計算量
- **実測根拠**: NPS への影響は計測レベル以下と判断（TODO: 計測データへのリンク）

---

## 4. cfg パターンの不統一（既知の課題）

### 4.1 ティア階層が揃っていない

`layers.rs` は AVX512 > AVX2 > SSSE3 > SSE2 > WASM > scalar の6段階だが、
他のファイルはサブセットのみ実装。以下が代表的なギャップ:

| ギャップ | 影響 | 状況 |
|---|---|---|
| ~~`activation.rs`: AVX512 パスなし~~ | ~~AVX512 環境で AVX2 にフォールバック~~ | **解消済み** |
| ~~`network_halfkp/ka/ka_hm.rs`: WASM なし~~ | ~~WASM ビルドでスカラー~~ | **解消済み** |

### 4.2 否定条件の書き方

3種類のパターンが混在:

```rust
// パターンA: not(上位ティア) を明示（推奨）
#[cfg(all(target_arch = "x86_64", target_feature = "avx2", not(target_feature = "avx512bw")))]

// パターンB: 排他を暗黙に任せる（layers.rs SSSE3 パス）
#[cfg(all(target_arch = "x86_64", target_feature = "ssse3", not(target_feature = "avx2")))]

// パターンC: return で制御フロー的に排他にする
#[cfg(all(target_arch = "x86_64", target_feature = "avx512f", ...))]
{ /* ... */ return; }
#[cfg(all(target_arch = "x86_64", target_feature = "avx2", not(...)))]
{ /* ... */ return; }
```

パターン A/B は cfg の排他性でコンパイル時に1つだけ有効になる。
パターン C は複数の cfg ブロックが同時にコンパイルされるが、`return` で実行時に排他になる。
`layer_stacks.rs` の `sqr_clipped_relu_transform` はパターン C を使用。

### 4.3 統一に向けた方針（未着手）

- 新規コードは `layers.rs` のパターン（パターン A/B の cfg 排他）に揃える
- 既存コードは動作に問題がないため、リファクタは棋力改善タスクより低優先
- WASM 対応の充足度は用途に応じて判断（ブラウザ対局が不要なら低優先）

---

## 5. 差分更新の実装状況

| レイヤー | 差分更新 | 備考 |
|---|---|---|
| Feature Transformer (1536次元) | **実装済み** | SIMD add/sub weights。玉移動時のみ full refresh |
| LayerStacks L1 (1536→16) | **なし (full forward)** | 入力が FT 出力そのものなので差分計算は意味がない |
| LayerStacks L2 (30→32) | **なし (full forward)** | 入力サイズが小さく差分追跡のオーバーヘッドが上回る |
| LayerStacks Output (32→1) | **なし (full forward)** | 同上 |

差分更新が有効なのは Feature Transformer レベルのみ。
バケット内の L1/L2/Output は入力サイズが小さく、毎回 full forward で問題ない。
