# Rust エンジン PGOベンチマーク更新

PGOビルドを実行し、`packages/rust-core/docs/performance/README.md` のPGO関連セクションを最新の計測結果で更新します。

## 処理内容

1. **PGOビルドの実行**
   ```bash
   cd packages/rust-core
   ./scripts/build_pgo.sh
   ```

2. **ベンチマーク実行（3回）**
   ```bash
   # NNUE評価時
   ./target/release/benchmark
   ./target/release/benchmark
   ./target/release/benchmark

   # Material評価時（オプション）
   MATERIAL_LEVEL=9 ./target/release/benchmark
   MATERIAL_LEVEL=9 ./target/release/benchmark
   MATERIAL_LEVEL=9 ./target/release/benchmark
   ```

3. **結果の計算**
   - 各Run のAvg NPSを記録
   - 3回の平均を計算
   - PGO前（通常ビルド）との向上率を計算

4. **ドキュメント更新**
   `packages/rust-core/docs/performance/README.md` の以下のセクションを更新:

   **「PGOビルド（本番用）」テーブル**:
   - PGO前のNPS（通常ビルドの値を参照）
   - PGO後のNPS
   - YaneuraOu比
   - 向上率

   **「PGO効果」セクション**:
   - NNUE評価時のRun 1/2/3と平均NPS
   - Material評価時のRun 1/2/3と平均NPS（計測した場合）
   - NPS向上率、絶対値向上

   **「変更履歴」セクション**:
   - PGO計測結果の更新を簡潔に記載（例: 「PGO計測結果更新（NNUE: xxx,xxx NPS、+x.x%）」）

## 実行

上記の処理を順番に実行し、README.mdを更新してください。

注意:
- 前回のPGO計測値との差分分析は不要（純粋にNPS値を更新するだけ）
- NPSの値はカンマ区切りで記載（例: 723,855）
- 向上率は小数点1桁まで記載（例: +6.2%）
- YaneuraOu比は整数%で記載（例: 65%）
- PGO前の値は「NPS計測結果」セクションの通常ビルド値を参照
