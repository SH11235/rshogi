# WASM パフォーマンス計測レポート

Web/WASM 版のベンチ計測結果を記録する。
Rust 側の `packages/rust-core/docs/performance/README.md` の形式に合わせた簡易版。

## 計測環境

| 項目 | 値 |
|------|-----|
| CPU | AMD Ryzen 9 5950X 16-Core Processor |
| コア数 | 32 |
| OS | Linux |
| アーキテクチャ | x64 |
| 計測ツール | `packages/engine-wasm/scripts/bench-wasm.mjs` |

## 計測条件

- Threads: 1（WASM）
- TT: 64MB
- Limit: nodes=1,000,000
- Iterations: 1（warmup 0）
- 局面: YaneuraOu準拠4局面（`hirate-like`, `complex-middle`, `tactical`, `movegen-heavy`）

---

## NNUE 有効時

計測日: 2025-12-23T04:20:51Z
計測コマンド:

```bash
pnpm --filter @shogi/engine-wasm bench:wasm -- --nnue-file ../rust-core/memo/YaneuraOu/eval/nn.bin
```

NNUE ファイル: `packages/rust-core/memo/YaneuraOu/eval/nn.bin`

### 集計

| 指標 | 値 |
|------|-----|
| 合計ノード数 | 4,000,000 |
| 合計時間 | 12,906ms |
| 平均NPS | 309,932 |
| 平均探索深さ | 14.50 |
| 平均hashfull | 138.50 |

### 局面別

| 局面 | depth | nodes | time_ms | nps | hashfull | bestmove |
|------|-------|-------|---------|-----|----------|----------|
| hirate-like | 16 | 1,000,000 | 2,587 | 386,548 | 19 | 2e2d |
| complex-middle | 15 | 1,000,000 | 3,800 | 263,157 | 113 | 8d8f |
| tactical | 13 | 1,000,000 | 3,361 | 297,530 | 180 | S*4a |
| movegen-heavy | 14 | 1,000,000 | 3,158 | 316,656 | 242 | G*2h |

---

## Material 評価時（NNUE 無効）

計測日: 2025-12-23T04:21:13Z
計測コマンド:

```bash
pnpm --filter @shogi/engine-wasm bench:wasm -- --material
```

### 集計

| 指標 | 値 |
|------|-----|
| 合計ノード数 | 4,000,000 |
| 合計時間 | 12,785ms |
| 平均NPS | 312,866 |
| 平均探索深さ | 15.00 |
| 平均hashfull | 177.00 |

### 局面別

| 局面 | depth | nodes | time_ms | nps | hashfull | bestmove |
|------|-------|-------|---------|-----|----------|----------|
| hirate-like | 14 | 1,000,000 | 2,720 | 367,647 | 45 | 2h2f |
| complex-middle | 16 | 1,000,000 | 3,436 | 291,036 | 133 | 8d7d |
| tactical | 15 | 1,000,000 | 3,273 | 305,530 | 227 | S*6a |
| movegen-heavy | 15 | 1,000,000 | 3,356 | 297,973 | 303 | G*3c |

---

## 並列探索ベンチマーク（ネイティブ版）

計測日: 2025-12-24
計測コマンド:

```bash
cd packages/rust-core
RUSTFLAGS="-C target-cpu=native" cargo build --release
./target/release/benchmark \
  --engine ./target/release/engine-usi \
  --threads 1,2,4,8 \
  --limit-type movetime \
  --limit 5000 \
  --tt-mb 512
```

### NNUE評価 - NPS比較

| 局面 | 1スレッド | 2スレッド | 4スレッド | 8スレッド |
|------|-----------|-----------|-----------|-----------|
| hirate-like | 578,853 | 1,133,615 | 1,612,023 | 3,424,602 |
| complex-middle | 413,408 | 749,531 | 1,585,273 | 2,212,078 |
| tactical | 445,760 | 579,640 | 1,669,592 | 2,756,123 |
| movegen-heavy | 446,374 | 811,199 | 1,252,278 | 2,686,478 |

### NNUE評価 - 探索深さ

| 局面 | 1スレッド | 2スレッド | 4スレッド | 8スレッド |
|------|-----------|-----------|-----------|-----------|
| hirate-like | 15 | 16 | 18 | 19 |
| complex-middle | 17 | 18 | 18 | 19 |
| tactical | 16 | 16 | 16 | 17 |
| movegen-heavy | 16 | 17 | 16 | 17 |

### Material評価 - NPS比較

| 局面 | 1スレッド | 2スレッド | 4スレッド | 8スレッド |
|------|-----------|-----------|-----------|-----------|
| hirate-like | 575,692 | 1,074,132 | 1,658,714 | 2,799,023 |
| complex-middle | 418,034 | 763,051 | 1,314,938 | 2,740,824 |
| tactical | 448,512 | 845,602 | 1,746,267 | 2,660,318 |
| movegen-heavy | 435,112 | 623,477 | 1,676,106 | 2,336,612 |

### スケーラビリティ

| スレッド数 | 理想倍率 | NNUE実測倍率 | NNUE効率 | Material実測倍率 | Material効率 |
|------------|----------|--------------|----------|------------------|--------------|
| 2 | 2.0x | 1.74x | 86.9% | 1.75x | 87.5% |
| 4 | 4.0x | 3.25x | 81.2% | 3.44x | 86.0% |
| 8 | 8.0x | 5.88x | 73.5% | 5.68x | 71.0% |

### 総合まとめ

| 評価関数 | 1スレッド平均NPS | 8スレッド平均NPS | 8スレッド倍率 |
|----------|------------------|------------------|---------------|
| Material | 469,338 | 2,634,194 | 5.61x |
| NNUE | 471,052 | 2,769,515 | 5.88x |

**備考**: 並列探索はLazy SMP方式で実装。8スレッドで約5.7〜5.9倍のNPS向上を達成。WASM版では並列探索は未対応（シングルスレッドのみ）。

---

## 変更履歴

| 日付 | NNUE平均NPS | Material平均NPS | 内容 |
|------|----------:|---------------:|------|
| 2025-12-21 | 310,824 | 314,712 | 初回計測 |
| 2025-12-23 | 309,932 | 312,866 | **board_effect機能追加**（fix-material-board_effectブランチ）。Material評価で利きの情報を使用する機能を追加。NNUE評価時はboard_effectを使わない設計により、NPSへの影響は誤差範囲（NNUE: -0.3%、Material: -0.6%）に抑制。評価精度向上とパフォーマンス維持を両立 |
| 2025-12-24 | - | - | **並列探索実装**（parallel-searchブランチ）。Lazy SMP方式による並列探索を実装。8スレッドでNNUE: 5.88x、Material: 5.61xのスケーラビリティを達成 |
