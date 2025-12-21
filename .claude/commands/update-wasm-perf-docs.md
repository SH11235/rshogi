# WASM パフォーマンスドキュメント更新

`packages/engine-wasm/docs/performance/README.md` を最新の計測結果で更新します。

## 手順

1. **ベンチマークの実行**

   モノレポのルートディレクトリから以下のコマンドを実行します。

   ```bash
   # NNUE有効時
   pnpm --filter @shogi/engine-wasm bench:wasm -- --nnue-file packages/rust-core/memo/YaneuraOu/eval/nn.bin

   # Material評価時
   pnpm --filter @shogi/engine-wasm bench:wasm -- --material
   ```

   出力はJSON形式です。

2. **結果の読み取り**

   JSON出力から以下の情報を抽出:
   - `system_info`: 計測環境（CPU、OS等）
   - `eval_info.nnue_enabled`: NNUEの有効/無効
   - `results[0].results`: 各局面の計測結果（depth, nodes, time_ms, nps, hashfull, bestmove）

3. **集計値の計算**

   4局面分の結果から以下を集計:
   - 合計ノード数
   - 合計時間
   - 平均NPS（合計ノード数 / 合計時間 × 1000）
   - 平均探索深さ
   - 平均hashfull

4. **ドキュメント更新**

   `packages/engine-wasm/docs/performance/README.md` を更新:
   - 計測環境（CPU、コア数、OS、アーキテクチャ）
   - NNUE有効時セクション:
     - 計測日（ISOタイムスタンプ）
     - 集計テーブル
     - 局面別テーブル
   - Material評価時セクション:
     - 計測日（ISOタイムスタンプ）
     - 集計テーブル
     - 局面別テーブル

## 実行

1. 上記のベンチマークコマンドを順次実行
2. 各JSON出力を解析し、ドキュメントを更新

## 注意事項

- ベンチマーク実行前に `pnpm --filter @shogi/engine-wasm build` でビルドが完了していること
- NNUEファイルのパスは環境に応じて調整
- 数値はカンマ区切りで記載（例: 1,000,000）

## 関連コマンド

Rust native版のパフォーマンス計測は `/update-rust-perf-docs` を使用してください。
