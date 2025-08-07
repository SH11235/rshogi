# NNUE学習データ生成

このドキュメントでは、NNUE（Efficiently Updatable Neural Network）評価関数用の学習データを生成する方法について説明します。

## 概要

`generate_nnue_training_data`ツールは、SFEN局面ファイルを処理し、各局面に対して探索を実行して評価データを生成します。新しいUnifiedSearcherフレームワークの`Material`エンジンタイプを使用し、効率的なデータ生成のために並列処理を実装しています。

### 主な特徴

- **段階的な探索深度設定**: 初期データ収集では浅い探索（深度2）、品質向上では深い探索（深度4以上）
- **レジューム機能**: 処理が中断しても、自動的に続きから再開
- **スキップ局面の保存**: タイムアウトした局面を別ファイルに保存し、後で再処理可能
- **進捗追跡**: `.progress`ファイルで実際の処理進捗を管理（スキップ分も含む）
- **並列処理**: Rayonを使用してすべてのCPUコアを活用

### なぜMaterialエンジンタイプを使用するのか

学習データ生成には`Material`エンジンタイプが最適です：

1. **高速な評価**: 駒価値のみの評価で、NNUEの読み込みが不要
2. **一貫性**: すべての局面で同じ評価基準を使用
3. **効率性**: 大量の局面を短時間で処理可能
4. **安定性**: シンプルな評価関数のため、エラーが少ない

## 前提条件

1. 最適なパフォーマンスのために、リリースモードでツールをビルドします：
```bash
cd packages/rust-core
cargo build --release --bin generate_nnue_training_data
```

## 使用方法

### 実行ディレクトリ

`packages/rust-core` ディレクトリから実行してください：

```bash
cd packages/rust-core
```

### 基本コマンド

```bash
./target/release/generate_nnue_training_data <入力SFENファイル> <出力学習データファイル> [深度] [バッチサイズ] [再開位置]
```

パラメータ：
- `深度`: 探索深度（デフォルト: 2、範囲: 1-10）
- `バッチサイズ`: 並列処理する局面数（デフォルト: 50）
- `再開位置`: 処理を再開する行番号（デフォルト: 0 = 自動検出）

### 段階的なデータ生成（推奨）

```bash
# Stage 1: 大量の浅い探索データ（深度2）
./target/release/generate_nnue_training_data input.sfen output_d2.txt 2 100

# Stage 2: 中程度の探索データ（深度3）
./target/release/generate_nnue_training_data input.sfen output_d3.txt 3 50

# Stage 3: 高品質データ（深度4以上は慎重に）
./target/release/generate_nnue_training_data input.sfen output_d4.txt 4 25
```

### レジューム機能の使用

```bash
# 初回実行
./target/release/generate_nnue_training_data input.sfen output.txt 3 50

# 中断後の再開（自動的に続きから処理）
./target/release/generate_nnue_training_data input.sfen output.txt 3 50

# 明示的に再開位置を指定
./target/release/generate_nnue_training_data input.sfen output.txt 3 50 10000
```

### スキップされた局面の再処理

```bash
# スキップされた局面は自動的に別ファイルに保存される
# output_skipped.txt - スキップされた局面
# output.progress - 進捗追跡ファイル

# スキップされた局面を再処理（深度を上げて、バッチサイズを小さく）
./target/release/generate_nnue_training_data output_skipped.txt output_retry.txt 4 10
```

## 入力形式

入力ファイルは、1行に1つのSFEN局面を含む必要があります：
```
sfen +Bn1g2s1l/2skg2r1/ppppp1n1p/5bpp1/5p1P1/2P6/PP1PP1P1P/1SK2S1R1/LN1G1G1NL w Lp 24
sfen +R1G4nl/1g4+Ss1/1kspp2p1/ppp2pS1p/4n4/P4Gp1P/1P1PP1P2/1+n2K2R1/7NL w G2P2b2lp 24
```

## 出力形式

### メインの出力ファイル
評価値付きのSFEN局面が含まれます：
```
+Bn1g2s1l/2skg2r1/ppppp1n1p/5bpp1/5p1P1/2P6/PP1PP1P1P/1SK2S1R1/LN1G1G1NL w Lp 24 eval -385 # d4
+R1G4nl/1g4+Ss1/1kspp2p1/ppp2pS1p/4n4/P4Gp1P/1P1PP1P2/1+n2K2R1/7NL w G2P2b2lp 24 eval 160 # d4
```

メタデータ：
- `# d4` - 正常に深度4まで探索完了
- `# timeout_d3` - 深度3でタイムアウト

### スキップファイル（_skipped.txt）
タイムアウトした局面の詳細情報：
```
sfen l1s1k2nl/1r1g2g2/2npppsp1/ppp3p1p/9/P1P2P1PP/1PSPPSP2/2G4R1/LN2KG1NL w Bb 24 # position 30 timeout 1.3s depth_reached 3
```

### 進捗ファイル（.progress）
実際に処理を試みた局面数（成功・スキップ両方含む）：
```
50
```

## パフォーマンス

### 探索深度別の時間制限
- 深度1: 50ms
- 深度2: 100ms
- 深度3: 200ms
- 深度4: 400ms
- 深度5以上: 800ms

### 処理速度の目安
- 深度2: 約50-100局面/秒
- 深度3: 約20-50局面/秒
- 深度4: 約10-20局面/秒

## トラブルシューティング

### 処理が遅い・タイムアウトが多い場合

1. **探索深度を下げる**：
```bash
# 深度2で高速処理
./target/release/generate_nnue_training_data input.sfen output.txt 2 100
```

2. **バッチサイズを小さくする**：
```bash
# 25局面ずつ処理（より安定）
./target/release/generate_nnue_training_data input.sfen output.txt 3 25
```

3. **スレッド数を制限する**：
```bash
# スレッド数を4に制限
RAYON_NUM_THREADS=4 ./target/release/generate_nnue_training_data input.sfen output.txt
```

### レジューム時の注意点

レジューム機能は以下の3つのファイルを使用します：
- **出力ファイル**: 成功した結果のみ
- **スキップファイル**: タイムアウトした局面
- **進捗ファイル**: 実際の処理進捗

これらのファイルの整合性が保たれるよう、処理中はファイルを手動で編集しないでください。

### デバッグログ

詳細なログを出力する場合：
```bash
RUST_LOG=debug ./target/release/generate_nnue_training_data input.sfen output.txt
```

## 技術詳細

### エンジン設定
- エンジンタイプ：`Material`（駒価値評価）
- 探索アルゴリズム：基本的なアルファベータ探索
- トランスポジションテーブル：8MB

### 並列処理
- Rayonを使用した自動並列化
- バッチ単位での処理でメモリ効率を向上
- 各バッチの結果は即座にファイルに書き込み

### タイムアウト処理
- 設定時間の2倍を超えた場合はスキップ
- スキップされた局面は別ファイルに保存
- 後で異なる設定で再処理可能

## 次のステップ

学習データを生成した後、以下の作業が必要です：

1. スキップされた局面の再処理（必要に応じて）
2. 専用ツールを使用してデータをNNUEバイナリ形式に変換
3. NNUE学習ツールを使用してニューラルネットワークの重みを作成

## 関連ドキュメント

- [エンジンタイプ選択ガイド](../../engine-core/docs/engine-type-selection.md)
- [UnifiedSearcherフレームワーク](../../docs/unified-searcher-design.md)