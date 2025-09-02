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
- **`generate_nnue_training_data`** - NNUE学習用データの生成（エンジン選択/NNUE重み/CP・WDL・Hybridラベル、スキップ・レジューム対応）

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
# 基本（深さ2、バッチ50、Material評価）
./target/release/generate_nnue_training_data input.sfen out_d2.txt 2 100 --engine material --hash-mb 16

# 品質重視（Enhanced、深さ3）
./target/release/generate_nnue_training_data input.sfen out_enh_d3.txt 3 50 --engine enhanced --hash-mb 16

# NNUE重みを指定してWDLラベル出力
./target/release/generate_nnue_training_data input.sfen out_nnue_wdl.txt 3 50 \
  --engine nnue --nnue-weights path/to/weights.nnue \
  --label wdl --wdl-scale 600

# ハイブリッド（序中盤WDL/終盤CP）
./target/release/generate_nnue_training_data input.sfen out_hybrid.txt 3 50 \
  --engine enhanced --label hybrid --hybrid-ply-cutoff 100

# レジューム機能（自動的に続きから処理）
./target/release/generate_nnue_training_data input.sfen out_enh_d3.txt 3 50

# 明示的にレジューム位置を指定
./target/release/generate_nnue_training_data input.sfen out_enh_d3.txt 3 50 5000

# スキップされた局面は自動的に別ファイルに保存される
# out_enh_d3_skipped.txt - スキップされた局面
# out_enh_d3.progress      - 進捗追跡ファイル
```

主要オプション一覧:
- `--engine material|enhanced|nnue|enhanced-nnue`（既定: material）
- `--nnue-weights <path>`（NNUE系選択時に任意）
- `--label cp|wdl|hybrid`（既定: cp）/ `--wdl-scale <float>` / `--hybrid-ply-cutoff <u32>`
- `--time-limit-ms <u64>`（深さ既定値の上書き）
- `--hash-mb <usize>`（TTサイズ、推奨 16）

### 定跡データの検証
```bash
./target/release/verify_opening_book opening_book.bin
```

### ベンチマークの実行
```bash
./target/release/shogi_benchmark
./target/release/nnue_benchmark
```
