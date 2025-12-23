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

## 変更履歴

| 日付 | NNUE平均NPS | Material平均NPS | 内容 |
|------|----------:|---------------:|------|
| 2025-12-21 | 310,824 | 314,712 | 初回計測 |
| 2025-12-23 | 309,932 | 312,866 | **board_effect機能追加**（fix-material-board_effectブランチ）。Material評価で利きの情報を使用する機能を追加。NNUE評価時はboard_effectを使わない設計により、NPSへの影響は誤差範囲（NNUE: -0.3%、Material: -0.6%）に抑制。評価精度向上とパフォーマンス維持を両立 |
