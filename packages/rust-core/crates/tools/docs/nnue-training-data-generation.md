# NNUE学習データ生成

このドキュメントでは、NNUE（Efficiently Updatable Neural Network）評価関数用の学習データを生成する方法について説明します。

## 概要

`generate_training_data`ツールは、SFEN局面ファイルを処理し、各局面に対して探索を実行して評価データを生成します。新しいUnifiedSearcherフレームワークの`Material`エンジンタイプを使用し、効率的なデータ生成のために並列処理を実装しています。

### なぜMaterialエンジンタイプを使用するのか

学習データ生成には`Material`エンジンタイプが最適です：

1. **高速な評価**: 駒価値のみの評価で、NNUEの読み込みが不要
2. **一貫性**: すべての局面で同じ評価基準を使用
3. **効率性**: 大量の局面を短時間で処理可能
4. **安定性**: シンプルな評価関数のため、エラーが少ない

`EnhancedNnue`などの高度なエンジンタイプは対局には優れていますが、学習データ生成には過剰です。

## 前提条件

1. 最適なパフォーマンスのために、リリースモードでツールをビルドします：
```bash
cd packages/rust-core
cargo build --release --bin generate_training_data
```

## 使用方法

### 実行ディレクトリ

`packages/rust-core` ディレクトリから実行してください：

```bash
cd packages/rust-core
```

### 基本コマンド

```bash
./target/release/generate_training_data <入力SFENファイル> <出力学習データファイル> [バッチサイズ] [再開行番号]
```

パラメータ：
- `バッチサイズ`: 並列処理する局面数（デフォルト: 25、推奨: 100）
- `再開行番号`: 処理を再開する行番号（デフォルト: 0 = 自動検出）

### 30,000局面の処理（24手目）

```bash
# デフォルト設定（100局面ずつ処理）
./target/release/generate_training_data crates/engine-cli/start_sfens_ply24.txt training_data_ply24.txt

# 大きめのバッチサイズ（500局面ずつ）
./target/release/generate_training_data crates/engine-cli/start_sfens_ply24.txt training_data_ply24.txt 500
```

### 30,000局面の処理（32手目）

```bash
# 安定性重視（100局面ずつ処理）
./target/release/generate_training_data crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt 100

# 中断後の再開（自動的に続きから）
./target/release/generate_training_data crates/engine-cli/start_sfens_ply32.txt training_data_ply32.txt 100
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
- バッチサイズ100で約1,000局面を10-15秒で処理
- 30,000局面の予想処理時間：5-10分

## 技術詳細

- 探索深度：8（250msタイムアウトで制限）
- タイムアウト局面の再試行：深度3（180msタイムアウト）
- 評価関数：`MaterialEvaluator`（駒価値ベース、高速評価）
- エンジンタイプ：`Material`（UnifiedSearcherフレームワーク使用）
- 並列処理：Rayonを使用してすべてのCPUコアを活用

## 次のステップ

学習データを生成した後、以下の作業が必要です：

1. 専用ツール（例：`make_kifu32bin`）を使用してデータをNNUEバイナリ形式に変換
2. NNUE学習ツールを使用してニューラルネットワークの重みを作成

## 主な特徴

- **中断・再開対応**：処理が途中で止まっても、同じコマンドで続きから再開
- **メモリ効率的**：バッチサイズごとに処理してファイルに書き込み
- **進捗表示**：各バッチの処理状況をリアルタイムで表示
- **柔軟な設定**：バッチサイズや再開位置を指定可能

## トラブルシューティング

### 処理が途中で止まる場合

1. **バッチサイズを小さくする**：
```bash
# 50局面ずつ処理（より安定）
./target/release/generate_training_data input.txt output.txt 50
```

2. **スレッド数を制限する**：
```bash
# スレッド数を4に制限
RAYON_NUM_THREADS=4 ./target/release/generate_training_data input.txt output.txt
```

3. **処理を再開する**：
```bash
# 同じコマンドを実行すると自動的に続きから処理
./target/release/generate_training_data input.txt output.txt
```

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

## アーキテクチャノート

このツールは、最新のUnifiedSearcherフレームワークに対応しています：

- `Engine::new(EngineType::Material)`を使用
- const genericsによる最適化：`UnifiedSearcher<MaterialEvaluator, true, false, 8>`
- トランスポジションテーブル: 8MB
- 基本的なアルファベータ探索（高度な枝刈りなし）

詳細は[エンジンタイプ選択ガイド](../../engine-core/docs/engine-type-selection.md)を参照してください。
