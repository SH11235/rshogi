# L1 AffineTransform sparse input 最適化の調査結果

調査期間: 2026-03-29
ブランチ: `perf/l1-sparse` (コミット `e8d61603`)

---

## 目的

LayerStack の L1 層 (1536→16) で、入力の非ゼロ chunk のみを処理する sparse 最適化を追加し NPS を改善する。
YO の `AffineTransformSparseInputExplicit` に準拠した実装。

## 背景

- L1 入力 (u8[1536]) の 4-byte chunk ゼロ率: **30%** (v82-300 モデル, depth 20, 370K calls で安定)
- dense ループ: 384 iterations (NUM_INPUT_CHUNKS = 1536/4)
- OUTPUT_DIM = 16 → num_regs = 2 (AVX2 __m256i)
- 重みサイズ: 16 × 1536 = 24,576 bytes (L1 32KB に収まる)
- スクランブル形式: weights[input_chunk][output][4]、各チャンク = 64 bytes = 1 cache line

## 試行した方式

### 方式 1: find_nnz + lookup table (YO 準拠)

AVX2 で 8 chunks ずつ非ゼロ検出し、256エントリ lookup table (4KB) でインデックス展開。
非ゼロ chunk のインデックス配列を構築した後、そのインデックスでのみ matmul を実行。

```
find_nnz: 48 SIMD iterations (384/8)
  → _mm256_load → _mm256_cmpgt_epi32 → _mm256_movemask_ps
  → lookup_indices[mask] → _mm_storeu_si128 → popcount

sparse matmul: ~269 iterations (384 × 0.70)
  → nnz_indices[j] → _mm256_set1_epi32 → 2× m256_add_dpbusd_epi32
```

#### ベンチマーク結果 (search_only_ab, 4 rounds, --cpus 2,4)

```
engine       runs    avg_nps    cycles/node    instructions/node
baseline       32     533336         8115.8              16066.9
candidate      32     526375         8246.2              15208.9

candidate vs baseline: NPS -1.31%, cycles/node +1.61%, instructions/node -5.34%
```

tree-safe: 4/4 全 depth 完全一致 (depth 20, Hash 256MB)

### 方式 2: インライン branch

dense ループ内で `if chunk_val != 0 { continue; }` で単純にスキップ。

```
candidate vs baseline: NPS -16.03%, instructions/node -0.19%
```

ループ内の data-dependent branch がパイプラインを完全に破壊。即棄却。

## perf 計測結果

### L1 キャッシュ・ブランチ (perf stat, movetime 10s, taskset -c 2)

| カウンタ | baseline | candidate | 差分 |
|---------|----------|-----------|------|
| L1-dcache miss rate | 3.45% | 3.49% | ±0 |
| branch miss rate | 8.33% | 8.28% | ±0 |

**結論**: lookup table (4KB) による L1 キャッシュ圧迫は発生していない。ブランチ予測にも悪影響なし。

### dispatch stall・HW prefetch (perf stat, movetime 10s, taskset -c 2)

| カウンタ | baseline | candidate | 差分 | 解釈 |
|---------|----------|-----------|------|------|
| L1-dcache-prefetches | 3.57B | 2.57B | **-28%** | HW prefetch が追従不能 |
| load_queue_rsrc_stall | 3.28B | 4.06B | **+24%** | load-to-use hazard |
| store_queue_rsrc_stall | 832M | 1.41B | **+69%** | find_nnz の _mm_storeu_si128 |
| IPC | 2.04 | 1.88 | **-7.8%** | 上記の複合効果 |
| instructions | 100.76B | 93.20B | **-7.5%** | sparse によるスキップ効果 |
| cycles | 49.40B | 49.45B | ±0% | 命令削減と IPC 劣化が相殺 |

## IPC 劣化の原因分析（計測で確認済み）

### 1. HW prefetch の喪失 (-28%)

dense ループは `weights_ptr + i * 64` の定数ストライドアクセスで、HW prefetcher が次のキャッシュラインを事前にロードできる。
sparse ループは `nnz_indices[j]` による間接アドレッシングのため、次のアクセス先が予測不能になり prefetch が効かない。

### 2. load queue stall の増加 (+24%)

sparse ループの依存チェーン:
```
nnz_indices[j] をロード (4-5 cycle latency)
  → i に基づいて weights_ptr + i*64 のアドレスを計算
    → _mm256_load_si256 は前のロード完了まで発行不能
```

dense ループではアドレスがループ変数から直接計算されるため、この依存チェーンが存在しない。

### 3. store queue stall の増加 (+69%)

find_nnz 内の `_mm_storeu_si128` が nnz_indices 配列をスタックに書き出す。
48 回の SSE2 ストア (48 × 16 bytes = 768 bytes) がストアバッファを圧迫。

## 結論

- instructions は 5-7% 確実に削減される（sparse skip の効果）
- しかし IPC が 7.8% 劣化し、NPS 改善には至らない
- IPC 劣化の主因は HW prefetch 喪失 (-28%) と load queue stall (+24%)
- **現在のモデル (30% ゼロ率, OUTPUT_DIM=16) では sparse 最適化は損益分岐に届かない**

## 将来の展望

以下の条件で再評価の価値がある:

1. **ゼロ率 50%+**: FT 出力の ReLU/SCReLU でカットされる割合が増えれば、スキップ量が損益分岐を超える
2. **OUTPUT_DIM の増加**: num_regs が増えると per-chunk の dpbusd 回数が増え、1 chunk スキップの利益が大きくなる（現在 num_regs=2 は最小ケース）
3. **INPUT_DIM の増加**: find_nnz のオーバーヘッドが INPUT_DIM に対してサブリニアなため、大きい入力ほど有利

実装はブランチ `perf/l1-sparse` のコミット `e8d61603` に保存されており、cherry-pick で再利用可能。
