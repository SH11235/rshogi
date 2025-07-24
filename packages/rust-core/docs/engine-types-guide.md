# エンジンタイプ選択ガイド

## 概要

本将棋エンジンは4つのエンジンタイプを提供しており、用途に応じて選択できます。

## エンジンタイプ一覧

| タイプ | 探索アルゴリズム | 評価関数 | 推奨用途 |
|--------|-----------------|----------|----------|
| **EnhancedNnue** | Enhanced (高度な枝刈り) | NNUE | **最強設定・対局** |
| **Nnue** | Basic (シンプル) | NNUE | 高速分析・検証 |
| **Enhanced** | Enhanced (高度な枝刈り) | 駒割り | 軽量環境・学習 |
| **Material** | Basic (シンプル) | 駒割り | デバッグ・テスト |

## 詳細説明

### EnhancedNnue（推奨）
- **最強の組み合わせ**
- 高度な探索技術（Null Move Pruning、LMR、Futility Pruning等）
- NNUE評価関数による精密な局面評価
- トランスポジションテーブル（16MB）による効率化
- 深い読みが可能（同じ時間でより多くの手を読める）

```
setoption name EngineType value EnhancedNnue
```

### Nnue
- シンプルな探索 + NNUE評価
- 安定した動作
- 評価関数の性能を純粋に活用
- 浅い探索では高速

```
setoption name EngineType value Nnue
```

### Enhanced
- 高度な探索技術 + シンプルな駒割り評価
- メモリ効率が良い（NNUE不要）
- 探索技術の学習に適している
- 軽量環境での使用に適している

```
setoption name EngineType value Enhanced
```

### Material
- 最もシンプルな実装
- デバッグやテストに最適
- 動作が予測可能
- 教育目的に適している

```
setoption name EngineType value Material
```

## 性能比較（目安）

同じ思考時間での相対的な強さ（Material = 1.0として）：

| エンジンタイプ | 相対強度 | 探索深度 | メモリ使用量 |
|---------------|---------|----------|--------------|
| EnhancedNnue | 4.0-5.0 | 深い（10-15手） | 大（200MB+） |
| Nnue | 2.5-3.0 | 標準（6-10手） | 中（170MB） |
| Enhanced | 2.0-2.5 | 深い（8-12手） | 小（20MB） |
| Material | 1.0 | 標準（5-8手） | 最小（5MB） |

## 使用シーン別推奨設定

### 対局・大会
```
setoption name EngineType value EnhancedNnue
setoption name USI_Hash value 256
setoption name Threads value 4
```

### 高速分析
```
setoption name EngineType value Nnue
setoption name USI_Hash value 128
setoption name Threads value 2
```

### 省メモリ環境
```
setoption name EngineType value Enhanced
setoption name USI_Hash value 16
setoption name Threads value 1
```

### デバッグ・開発
```
setoption name EngineType value Material
setoption name USI_Hash value 16
setoption name Threads value 1
```

## 技術詳細

### Enhanced探索の特徴
- **Null Move Pruning**: 相手に2手連続で指させて早期枝刈り
- **Late Move Reductions (LMR)**: 後半の手を浅く探索
- **Futility Pruning**: 明らかに悪い手を探索しない
- **Transposition Table**: 同一局面の探索結果をキャッシュ
- **History Heuristics**: 過去の探索で良かった手を優先
- **Aspiration Window**: 前回の評価値を基に探索窓を狭める

### NNUE評価関数の特徴
- **HalfKP 256x2-32-32-1アーキテクチャ**
- 王の位置を基準とした特徴量抽出
- 差分計算による高速更新
- 学習済み重みファイルが必要

## 注意事項

1. **スタックサイズ**: EnhancedNnueは大きなスタックを必要とする場合があります
   ```bash
   export RUST_MIN_STACK=8388608
   ```

2. **NNUE重みファイル**: Nnue/EnhancedNnueを使用する場合は重みファイルが必要です
   ```
   setoption name NNUEWeightFile value path/to/weights.nnue
   ```

3. **メモリ使用量**: EnhancedタイプはTransposition Tableのため追加メモリを使用します

## まとめ

- **最強を求める場合**: EnhancedNnue
- **バランス重視**: Nnue
- **軽量環境**: Enhanced
- **学習・デバッグ**: Material

基本的には**EnhancedNnue**の使用を推奨します。これは高度な探索技術とNNUE評価関数の組み合わせにより、最も強力な棋力を発揮します。