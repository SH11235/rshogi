# Debug Position Tool

## 概要

`debug_position` は将棋エンジンの特定局面での動作を調査するための汎用デバッグツールです。
探索性能の問題調査、手生成の検証、エンジンタイプの比較などに使用できます。

## 使い方

### 基本的な使用方法

```bash
# 特定局面の探索デバッグ
cargo run --release --bin debug_position -- --sfen "SFEN文字列" --depth 5 --time 1000

# 短縮形
cargo run --release --bin debug_position -- -s "SFEN文字列" -d 5 -t 1000
```

### オプション

- `--sfen, -s`: 解析する局面のSFEN文字列（省略時は初期局面）
- `--depth, -d`: 最大探索深さ（デフォルト: 5）
- `--time, -t`: 探索時間制限（ミリ秒、デフォルト: 1000）
- `--engine, -e`: エンジンタイプ（material/nnue/enhanced/enhanced_nnue、デフォルト: material）
- `--moves, -m`: 合法手一覧を表示
- `--perft, -p`: Perft解析を実行（手生成の正確性確認）
- `--show-ordering, -o`: 手順序情報を表示（将来実装予定）
- `--show-tt-stats`: トランスポジションテーブル統計（将来実装予定）

## 使用例

### 1. 特定局面での探索性能調査

```bash
# 問題のある局面842の調査（search-performance-investigation.mdで発見された問題）
cargo run --release --bin debug_position -- \
  --sfen "l5g1l/2s1k1s2/2npp1n1p/p4p1p1/1p2P4/P1P2PPP1/1PN1S1N1P/1B1GKG3/L6RL w BPrb2p 1" \
  --depth 4 \
  --time 300 \
  --engine material
```

### 2. エンジンタイプの比較

```bash
# Material エンジンでの解析
cargo run --release --bin debug_position -- -s "SFEN" -e material -d 6

# NNUE エンジンでの解析
cargo run --release --bin debug_position -- -s "SFEN" -e nnue -d 6

# Enhanced NNUE（最強設定）での解析
cargo run --release --bin debug_position -- -s "SFEN" -e enhanced_nnue -d 6
```

### 3. 手生成の検証

```bash
# 合法手一覧の確認
cargo run --release --bin debug_position -- -s "SFEN" --moves

# Perft解析（手生成の正確性確認）
cargo run --release --bin debug_position -- -s "SFEN" --perft 5
```

### 4. タイムアウト問題の調査

```bash
# 短い時間制限で探索が正常に終了するか確認
cargo run --release --bin debug_position -- -s "SFEN" -d 8 -t 100
```

## 出力例

```
Analyzing position: l5g1l/2s1k1s2/2npp1n1p/p4p1p1/1p2P4/P1P2PPP1/1PN1S1N1P/1B1GKG3/L6RL w BPrb2p 1
Using engine type: Material
Max depth: 4
Time limit: 300ms

Starting search...

=== Search Results ===
Best move: 7f7e
Score: -123
Depth reached: 4
Nodes searched: 45678
Time: 0.287s
NPS: 159229
PV: 7f7e 8d8e 7e7d 8e8f
```

## Claude Codeへの注意事項

このツールは以下の場面で使用してください：

1. **探索性能の問題調査時**
   - 特定局面で探索が遅い/ハングする問題の調査
   - 深さごとの探索時間の計測

2. **エンジンの動作確認時**
   - 新しい機能実装後の動作確認
   - リファクタリング後の性能比較

3. **手生成の検証時**
   - 特定局面での合法手確認
   - Perft値の確認

4. **バグ調査時**
   - 特定局面での異常動作の再現
   - エンジンタイプ間の動作差異の確認

## 関連ドキュメント

- [Search Performance Investigation](./search-performance-investigation.md) - このツールを使用した実際の調査例
- [Engine Type Selection](../engine-core/docs/engine-type-selection.md) - エンジンタイプの詳細