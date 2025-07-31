# NNUE学習データ生成

このドキュメントでは、NNUE（Efficiently Updatable Neural Network）評価関数用の学習データを生成する方法について説明します。

## 概要

`generate_training_data`ツールは、SFEN局面ファイルを処理し、各局面に対して浅い探索（深さ4）を実行して評価データを生成します。効率的なデータ生成のために並列処理を使用しています。

## 前提条件

1. 最適なパフォーマンスのために、リリースモードでツールをビルドします：
```bash
cd packages/rust-core
cargo build --release --bin generate_training_data --bin generate_training_data_batch --bin debug_training_data
```

## 使用方法

### 実行ディレクトリ

`packages/rust-core` ディレクトリから実行してください：

```bash
cd packages/rust-core
```

### 基本コマンド

```bash
./target/release/generate_training_data <入力SFENファイル> <出力学習データファイル>
```

### 30,000局面の処理（24手目）

```bash
./target/release/generate_training_data crates/engine-cli/start_sfens_ply24.txt training_data_ply24.txt
```

### 30,000局面の処理（32手目）

```bash
./target/release/generate_training_data crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt
```

## 入力形式

入力ファイルは、1行に1つのSFEN局面を含む必要があります：
```
sfen +Bn1g2s1l/2skg2r1/ppppp1n1p/5bpp1/5p1P1/2P6/PP1PP1P1P/1SK2S1R1/LN1G1G1NL w Lp 24
sfen +R1G4nl/1g4+Ss1/1kspp2p1/ppp2pS1p/4n4/P4Gp1P/1P1PP1P2/1+n2K2R1/7NL w G2P2b2lp 24
```

## 出力形式

出力ファイルには、評価値付きのSFEN局面が含まれます：
```
+Bn1g2s1l/2skg2r1/ppppp1n1p/5bpp1/5p1P1/2P6/PP1PP1P1P/1SK2S1R1/LN1G1G1NL w Lp 24 eval -605
+R1G4nl/1g4+Ss1/1kspp2p1/ppp2pS1p/4n4/P4Gp1P/1P1PP1P2/1+n2K2R1/7NL w G2P2b2lp 24 eval 132
```

## パフォーマンス

- 利用可能なすべてのCPUコアを使用した並列処理
- 約1,000局面を10-15秒で処理（時間制限を500msに増加）
- 30,000局面の予想処理時間：5-10分

## 技術詳細

- 探索深度：4
- 評価：駒価値ベース（データ生成用の高速評価）
- 局面あたりの時間制限：500ms（複雑な局面でも十分な探索を確保）
- 並列処理：Rayonを使用してすべてのCPUコアを活用

## 次のステップ

学習データを生成した後、以下の作業が必要です：

1. 専用ツール（例：`make_kifu32bin`）を使用してデータをNNUEバイナリ形式に変換
2. NNUE学習ツールを使用してニューラルネットワークの重みを作成

## トラブルシューティング

### 処理が途中で止まる場合

並列処理のスレッド数を制限して実行してください：
```bash
# スレッド数を8に制限
RAYON_NUM_THREADS=8 ./target/release/generate_training_data crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt

# さらに少なくする場合（4スレッド）
RAYON_NUM_THREADS=4 ./target/release/generate_training_data crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt
```

### バッチ処理版の使用

メモリ効率を改善したバッチ処理版も利用可能です：

```bash
# バッチ処理版のビルド
cargo build --release --bin generate_training_data_batch

# デフォルトバッチサイズ（1000）で実行
./target/release/generate_training_data_batch crates/engine-cli/start_sfens_ply24.txt training_data_ply24.txt

# バッチサイズを指定して実行（例：500局面ずつ処理）
./target/release/generate_training_data_batch crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt 500

# より大きなバッチサイズ（メモリに余裕がある場合）
./target/release/generate_training_data_batch crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt 2000
```

バッチ処理版の利点：
- メモリ使用量が一定に保たれる
- 各バッチごとに進捗表示
- 大規模なファイルでも安定して処理可能

### メモリ不足の場合

大きなファイルでメモリ問題が発生した場合：
- ファイルを小さなバッチに分割して処理
- システムのスワップ領域を増やす
- より多くのRAMを搭載したマシンを使用

### デバッグログ

詳細なログを出力する場合：
```bash
RUST_LOG=debug ./target/release/generate_training_data input.txt output.txt
```
