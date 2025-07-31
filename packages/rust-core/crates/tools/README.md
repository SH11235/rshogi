# ツール集

このクレートには、将棋エンジンプロジェクト用の各種ユーティリティツールが含まれています。

## 利用可能なツール

### 定跡関連ツール
- `convert_opening_book` - 定跡データの形式変換
- `verify_opening_book` - 定跡データの整合性確認
- `search_opening_book` - 定跡内の局面検索
- `sfen_hasher` - SFEN局面のハッシュ化

### NNUE関連ツール
- `create_mock_nnue` - テスト用のモックNNUE重み作成
- `nnue_benchmark` - NNUE評価関数のパフォーマンス測定
- **`generate_training_data`** - NNUE学習用データの生成

### ベンチマークツール
- `shogi_benchmark` - 将棋エンジン全般のベンチマーク
- `pv_benchmark` - 主要変化（PV）探索のベンチマーク
- `pv_simple_bench` - 簡易版PVベンチマーク
- `see_flamegraph` - 静的駒交換評価のフレームグラフ生成
- `simd_benchmark` - SIMD命令のベンチマーク
- `simd_check` - SIMD機能の確認
- `sse41_only_test` - SSE4.1機能のテスト

## ビルド

全ツールのビルド：
```bash
cargo build --release
```

特定のツールのビルド：
```bash
cargo build --release --bin <ツール名>
```

## ドキュメント

- [NNUE学習データ生成](docs/nnue-training-data-generation.md) - NNUE学習データ生成ガイド

## 使用例

### NNUE学習データの生成
```bash
# 24手目の30,000局面を処理（100局面ずつ）
./target/release/generate_training_data crates/engine-cli/start_sfens_ply24.txt training_data_ply24.txt

# 32手目の30,000局面を処理（500局面ずつ、メモリ効率重視）
./target/release/generate_training_data crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt 500

# 中断後の再開（同じコマンドで自動的に続きから）
./target/release/generate_training_data crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt 500
```

### 定跡データの検証
```bash
./target/release/verify_opening_book opening_book.bin
```

### ベンチマークの実行
```bash
./target/release/shogi_benchmark
./target/release/nnue_benchmark
```