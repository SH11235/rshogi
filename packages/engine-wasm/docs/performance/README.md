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

計測日: 2025-12-21T21:35:31Z  
計測コマンド:

```bash
pnpm --filter @shogi/engine-wasm bench:wasm -- --nnue-file ../rust-core/memo/YaneuraOu/eval/nn.bin
```

NNUE ファイル: `packages/rust-core/memo/YaneuraOu/eval/nn.bin`

### 集計

| 指標 | 値 |
|------|-----|
| 合計ノード数 | 4,000,001 |
| 合計時間 | 12,869ms |
| 平均NPS | 310,824 |
| 平均探索深さ | 14.25 |
| 平均hashfull | 147.25 |

### 局面別

| 局面 | depth | nodes | time_ms | nps | hashfull | bestmove |
|------|-------|-------|---------|-----|----------|----------|
| hirate-like | 15 | 1,000,000 | 2,456 | 407,166 | 28 | 1g1f |
| complex-middle | 15 | 1,000,000 | 3,666 | 272,776 | 113 | 8d8f |
| tactical | 12 | 1,000,001 | 3,346 | 298,864 | 192 | G*4c |
| movegen-heavy | 15 | 1,000,000 | 3,401 | 294,031 | 256 | G*2h |

---

## Material 評価時（NNUE 無効）

計測日: 2025-12-21T21:35:06Z  
計測コマンド:

```bash
pnpm --filter @shogi/engine-wasm bench:wasm -- --material
```

### 集計

| 指標 | 値 |
|------|-----|
| 合計ノード数 | 4,000,000 |
| 合計時間 | 12,710ms |
| 平均NPS | 314,712 |
| 平均探索深さ | 15.25 |
| 平均hashfull | 182.00 |

### 局面別

| 局面 | depth | nodes | time_ms | nps | hashfull | bestmove |
|------|-------|-------|---------|-----|----------|----------|
| hirate-like | 14 | 1,000,000 | 2,698 | 370,644 | 48 | 2h2f |
| complex-middle | 16 | 1,000,000 | 3,211 | 311,429 | 135 | B*6h |
| tactical | 16 | 1,000,000 | 3,613 | 276,778 | 230 | G*6b |
| movegen-heavy | 15 | 1,000,000 | 3,188 | 313,676 | 315 | G*1c |
