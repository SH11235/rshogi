# NNUE Accumulator 更新最適化タスク

## 背景

NNUE 評価では局面ごとに Accumulator（特徴量ベクトル）を計算する必要がある。
現在の更新フローは以下の3段階:

1. **直接差分更新**: 直前局面の Accumulator が計算済みなら差分適用（最速）
2. **祖先探索 + 複数手差分更新**: `find_usable_accumulator` で祖先を遡り、計算済み Accumulator から複数手分の差分を積み重ねる
3. **Full Refresh**: 盤面全体から Accumulator をゼロから計算（最遅）

`find_usable_accumulator` の `MAX_DEPTH` を 8→1 に変更した結果（コミット `4c063351`）、
NNUE NPS が約 4% 低下した（953,116 → 913,955）。

**重要**: どの経路で計算しても Accumulator の最終値はビット単位で同一であり、
探索木・評価値・指し手には一切影響しない。影響するのは NPS（速度）のみ。

---

## 方針A: MAX_DEPTH チューニング

### 概要

`find_usable_accumulator` の `MAX_DEPTH` をアーキテクチャごとに最適値に設定する。

### 理論的背景

| depth | diff コスト（L1 あたり） | refresh 対比 |
|:-----:|------------------------:|:------------|
| 1 | ~3 特徴量 × L1 | refresh の約 1/13 |
| 2 | ~6 特徴量 × L1 | refresh の約 1/7 |
| 4 | ~12 特徴量 × L1 | refresh の約 1/3 |
| 8 | ~24 特徴量 × L1 | refresh の約 3/5 |

※ refresh コスト ≈ ~40 特徴量 × L1

depth が増えるほど diff コストが refresh に近づき、さらにキャッシュミスのリスクも増える。

### アーキテクチャ別の影響予測

| アーキテクチャ | L1 | Acc サイズ | refresh の相対重さ | 予想最適 MAX_DEPTH |
|:-------------|---:|----------:|:-----------------:|:-----------------:|
| HalfKP 256x2-32-32 | 256 | ~2 KB | 中 | 1〜2 |
| HalfKP 512x2-32-32 | 512 | ~4 KB | 中〜高 | 2〜3 |
| HalfKA_hm 256 | 256 | ~2 KB | 中 | 1〜2 |
| LayerStacks 1536x2-15-32 | 1536 | ~12 KB | 非常に高 | 2〜4 |

L1 が大きいほど refresh コストが支配的になるため、MAX_DEPTH を上げる利得が大きい。
ただし Accumulator が大きいと祖先アクセス時のキャッシュミスコストも増える。

### 計測手順

1. `MAX_DEPTH` を変更可能にする（const → 設定可能 or feature flag）
2. 各アーキテクチャ × MAX_DEPTH=1,2,3,4 の組み合わせで NPS を計測
3. 計測条件: `--threads 1 --tt-mb 256 --limit-type movetime --limit 20000`
4. 結果テーブルから各アーキテクチャの最適値を決定

### 計測対象（優先順）

1. **HalfKP 256x2-32-32** (suisho5.bin) — 現在のメインモデル
2. **LayerStacks 1536x2-15-32** — L1 が大きく効果が期待される
3. その他のアーキテクチャ — 必要に応じて

### 実装方針

- 最もシンプルな案: アーキテクチャの L1 サイズに応じて MAX_DEPTH を自動決定
  ```
  L1 <= 256:  MAX_DEPTH = 計測結果から決定
  L1 <= 512:  MAX_DEPTH = 計測結果から決定
  L1 > 512:   MAX_DEPTH = 計測結果から決定
  ```
- `find_usable_accumulator` は全アーキテクチャで共通のコードパスを使用しているため、
  const generics パラメータか実行時の設定値として持たせる

### 計測結果（2026-03-26）

**LayerStack 1536x16x32 (v82-300, progress8kpabs)**

計測条件:
- `go depth 20`, Threads=1, Hash=256MB
- 15局面（実対局棋譜から ply 20/40/60/80/100 付近を各3局面抽出）
- 局面ソース: `runs/selfplay/20260325-v82_300-vs-aoba-fisher3m10s/0:v82-300-vs-1:AobaNNUE.jsonl`
- EvalFile: `checkpoints/v82/v82-300/quantised.bin` (FV_SCALE=28)
- 注: NNUE 学習プロセスが同時実行中のため絶対値は参考。相対比較は有効

| MAX_DEPTH | 平均 NPS | 対 MAX_DEPTH=1 比 |
|-----------|---------|-------------------|
| 1 (変更前) | 324,681 | 100% |
| 2 | 330,763 | +1.9% |
| 3 | 341,073 | +5.0% |
| **4** | **346,577** | **+6.7%** |

**結論**: LayerStack 1536x16x32 では MAX_DEPTH=4 が最適。+6.7% の NPS 改善。
L1=1536 の full refresh コストが高いため、4手分の祖先探索コストを払っても差分更新のほうが有利。

### Phase 2: AccumulatorCaches 計測結果（2026-03-26）

MAX_DEPTH=4 との組み合わせで計測。計測条件は Phase 1 と同一。

| 構成 | 平均 NPS | 対 MAX_DEPTH=1 比 |
|------|---------|-------------------|
| MAX_DEPTH=1 (ベースライン) | 324,681 | 100% |
| MAX_DEPTH=4 のみ | 346,577 | +6.7% |
| **MAX_DEPTH=4 + AccumulatorCaches** | **486,973** | **+50.0%** |

**AccumulatorCaches による追加改善: +40.5%**（MAX_DEPTH=4 単体比）。
full refresh 時にキャッシュからの差分更新（通常 2〜4 駒分）で済むため、
L1=1536 の高コスト refresh が大幅に削減された。

実装詳細:
- キャッシュ構造: `[81マス][2視点]` = 162 エントリ
- 各エントリ: アキュムレータ値 (1536×i16) + ソート済みアクティブ特徴インデックス (最大40個×u32)
- メモリ: 約 524 KB / スレッド
- 差分検出: ソート済み配列のマージベース O(n+m) アルゴリズム

### リスク・注意点

- 探索木への影響: **なし**（評価値は同一）
- YaneuraOu との乖離: MAX_DEPTH > 1 にすると YaneuraOu と挙動が異なるが、
  探索結果は同一なので問題なし
- キャッシュ効果は CPU 依存のため、異なる環境で再計測が望ましい

---

## 方針B: AccumulatorCaches（Finny Tables）導入

### 概要

Stockfish が 2024年4月に導入した仕組み（コミット `49ef4c93`、著者: gab8192）。
「Finny Tables」とも呼ばれ、Koivisto エンジンの Luecx が考案。

**核心的なアイデア**: Full Refresh 時に「ゼロから全駒を加算」するのではなく、
「前回同じ玉位置で計算した Accumulator からの差分」だけ適用する。

### 従来方式との比較

| | 従来の refresh | AccumulatorCaches |
|---|---|---|
| refresh 時の処理 | bias + 全特徴量（~40個）加算 | キャッシュからの差分（~2〜4個）加減算 |
| メモリ | なし | 玉位置 × 2色 のキャッシュテーブル |
| 効果 | - | refresh コストを大幅削減 |

### Stockfish の実装詳細

#### データ構造

```
AccumulatorCaches（スレッドごとに1つ）
└── entries: [SQUARE_NB][COLOR_NB] の CacheEntry 配列

CacheEntry（各玉位置 × 視点ごと）
├── accumulation: [L1] の i16 配列     ← 前回のアキュムレータ値
├── pieces: [SQUARE_NB] の Piece        ← 前回時点の駒配列
└── pieceBB: Bitboard                   ← 前回時点の駒ビットボード
```

- チェス: 64マス × 2色 = 128 エントリ
- **将棋: 81マス × 2色 = 162 エントリ**

#### アルゴリズム

```
refresh_with_cache(pos, perspective):
    ksq = pos.king_square(perspective)
    entry = cache[ksq][perspective]

    // SIMD で駒配列を一括比較し、変化マスを検出
    changed_bb = detect_changes(entry.pieces, pos.pieces)
    removed_bb = changed_bb & entry.pieceBB    // 消えた駒
    added_bb   = changed_bb & pos.pieces()     // 増えた駒

    // キャッシュの Accumulator に差分適用
    for sq in removed_bb:
        entry.accumulation -= weights[feature_index(ksq, entry.pieces[sq], sq)]
    for sq in added_bb:
        entry.accumulation += weights[feature_index(ksq, pos.piece_on(sq), sq)]

    // キャッシュを現在の局面で更新
    entry.pieces = pos.pieces
    entry.pieceBB = pos.pieces_bb

    // アキュムレータにコピー
    accumulator = entry.accumulation
```

#### なぜ効果が大きいか

αβ探索では同じ玉位置が繰り返し出現する。玉は頻繁に動かないため:
- 初回: フル構築（全駒加算）が必要
- 2回目以降: キャッシュとの差分は通常 2〜4 駒のみ

従来のフル refresh が ~40 特徴量の加算を要するのに対し、
キャッシュ付き refresh は ~4 特徴量の加減算で済む → **約10倍高速化**

### 将棋（rshogi）への適用

#### 必要な変更

1. **CacheEntry 構造体の追加**
   - `accumulation: [L1] の i16 配列`（両視点分）
   - `pieces: [SQ_NB] の Piece` または `PieceList` 相当
   - `occupied: Bitboard`（駒のある場所）

2. **キャッシュテーブル**
   - サイズ: 81マス × 2色 = 162 エントリ
   - メモリ（L1=256 の場合）: 162 × (256×2bytes + 81bytes + 16bytes) ≈ 100 KB/スレッド
   - メモリ（L1=1536 の場合）: 162 × (1536×2bytes + 81bytes + 16bytes) ≈ 512 KB/スレッド

3. **refresh_accumulator の改修**
   - 現在の「bias + 全特徴量」を「cache entry からの差分」に置換
   - 初回（キャッシュ未初期化）はフォールバックで従来通りフル構築

4. **駒配列の差分検出**
   - Stockfish は AVX2/NEON で 64 バイト一括比較
   - 将棋は 81 マスなので 128 バイト比較（AVX2 × 4 回 or NEON × 8 回）

#### HalfKP 固有の考慮事項

HalfKP では特徴量インデックスが `king_sq × BonaPiece` なので、
玉が動くと全特徴量インデックスが変わる → refresh が必要。
AccumulatorCaches はまさにこの refresh を高速化する仕組みなので、相性が良い。

ただし HalfKP の BonaPiece は「手駒」を含むため、盤上の駒配列だけでは差分検出が不完全。
手駒の変化も追跡する仕組みが必要。

#### 期待される効果

| 指標 | 現在 | 導入後（予測） |
|------|------|---------------|
| refresh 時のコスト | ~40 特徴量加算 | ~2〜4 特徴量加減算 |
| refresh 率 | ~33%（MAX_DEPTH=1） | ~33%（変わらず） |
| refresh の実時間 | 現在の 100% | 約 10〜20%（大幅削減） |
| 全体 NPS 改善 | - | +5〜15%（推定） |

### Stockfish の追加最適化（将来検討）

以下は Stockfish が 2025年2月に追加した最適化で、AccumulatorCaches の次のステップ:

- **Backward Update**: refresh 後のアキュムレータを過去方向に逆差分適用し、
  探索スタック上の未計算エントリを遡及的に確定させる
- **Double Incremental Update**: 2手連続の差分を融合して1回で適用

これらは AccumulatorCaches 導入後に段階的に検討する。

---

## 優先度と実施順序

### Phase 1: MAX_DEPTH チューニング（工数: 小）

- **目的**: 低コストで NPS 回復を狙う
- **工数**: 計測スクリプト調整 + ベンチマーク実行 + const 値変更
- **期待効果**: +2〜5% NPS（アーキテクチャ依存）
- **リスク**: ほぼなし

### Phase 2: AccumulatorCaches 導入（工数: 中〜大）

- **目的**: refresh コストの根本的な削減
- **工数**: 新データ構造 + refresh ロジック改修 + テスト + ベンチマーク
- **期待効果**: +5〜15% NPS（refresh 率とアーキテクチャに依存）
- **リスク**: 実装の正確性検証が必要（差分の加減算ミスは評価値のサイレント劣化を招く）
- **前提**: Phase 1 の計測結果で refresh コストの影響度を定量的に把握してから着手

### Phase 3: Backward Update / Double Inc Update（工数: 中）

- AccumulatorCaches 導入後に検討
- キャッシュヒット率の向上が期待できる

---

## 参考資料

- Stockfish コミット `49ef4c93` (2024-04-20): AccumulatorCaches 初期導入
- Stockfish コミット `e9997afb` (2025-02): Backward update 追加
- rshogi コミット `4c063351`: MAX_DEPTH 8→1 変更
- rshogi `docs/performance/README.md`: NNUE Accumulator 差分更新調査（2025-12-23）
  - 診断結果: diff_ok=76.0%, refresh=24.0%（MAX_DEPTH=8 時）
- Stockfish ソース: `/mnt/nvme1/development/Stockfish/src/nnue/nnue_accumulator.h`
