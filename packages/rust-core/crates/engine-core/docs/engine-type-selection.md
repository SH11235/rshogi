# エンジンタイプ選択ガイド

## 概要

engine-coreでは、統合検索フレームワーク（UnifiedSearcher）を使用して、4つの異なるエンジンタイプを提供しています。各エンジンタイプは、評価関数と探索アルゴリズムの組み合わせによって定義され、コンパイル時に最適化されます。

## エンジンタイプの詳細

### 1. Material（基本エンジン）
```rust
type MaterialSearcher = UnifiedSearcher<MaterialEvaluator, true, false, 8>;
```
- **評価関数**: 駒の価値のみ（MaterialEvaluator）
- **探索**: 基本的なアルファベータ探索
- **トランスポジションテーブル**: 8MB
- **枝刈り**: なし
- **用途**: デバッグ、テスト、学習用

### 2. Nnue（NNUE基本エンジン）
```rust
type NnueBasicSearcher = UnifiedSearcher<NNUEEvaluatorProxy, true, false, 8>;
```
- **評価関数**: ニューラルネットワーク（NNUE）
- **探索**: 基本的なアルファベータ探索
- **トランスポジションテーブル**: 8MB
- **枝刈り**: なし
- **用途**: 高速な解析、浅い探索

### 3. Enhanced（高度な探索エンジン）
```rust
type MaterialEnhancedSearcher = UnifiedSearcher<MaterialEvaluator, true, true, 16>;
```
- **評価関数**: 駒の価値のみ（MaterialEvaluator）
- **探索**: 高度な探索技術
  - Null Move Pruning
  - Late Move Reduction (LMR)
  - Futility Pruning
- **トランスポジションテーブル**: 16MB
- **枝刈り**: あり
- **用途**: メモリ制限環境、探索技術の学習

### 4. EnhancedNnue（最強エンジン）
```rust
type NnueEnhancedSearcher = UnifiedSearcher<NNUEEvaluatorProxy, true, true, 16>;
```
- **評価関数**: ニューラルネットワーク（NNUE）
- **探索**: 高度な探索技術（Enhancedと同様）
- **トランスポジションテーブル**: 16MB
- **枝刈り**: あり
- **用途**: 競技対局、最強の棋力

## const genericsパラメータの意味

```rust
UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>
```

1. **`E`**: 評価関数の型
   - `MaterialEvaluator`: 駒の価値による単純な評価
   - `NNUEEvaluatorProxy`: NNUE評価関数へのプロキシ

2. **`USE_TT`**: トランスポジションテーブルの使用
   - 全エンジンタイプで`true`（常に使用）

3. **`USE_PRUNING`**: 高度な枝刈り技術の使用
   - Basic系: `false`（単純なアルファベータ探索）
   - Enhanced系: `true`（複数の枝刈り技術を使用）

4. **`TT_SIZE_MB`**: トランスポジションテーブルのサイズ
   - Basic系: 8MB（軽量）
   - Enhanced系: 16MB（より多くの局面をキャッシュ）

## 実行時の切り替え

### controller.rsでの実装

```rust
pub enum EngineType {
    Material,
    Nnue,
    Enhanced,
    EnhancedNnue,
}

impl Engine {
    pub fn search(&self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        match self.engine_type {
            EngineType::Material => {
                // MaterialSearcherを使用
            }
            EngineType::Nnue => {
                // NnueBasicSearcherを使用
            }
            EngineType::Enhanced => {
                // MaterialEnhancedSearcherを使用
            }
            EngineType::EnhancedNnue => {
                // NnueEnhancedSearcherを使用
            }
        }
    }
}
```

### USIプロトコルでの設定

```
setoption name EngineType value EnhancedNnue
```

## パフォーマンス比較

同じ思考時間での相対的な強さ（Material = 1.0）：

| エンジンタイプ | 相対強度 | 探索深さ | メモリ使用量 |
|-------------|---------|---------|------------|
| Material | 1.0x | 基準 | ~13MB |
| Nnue | 2.5-3.0x | 浅い | ~178MB |
| Enhanced | 2.0-2.5x | 深い | ~29MB |
| EnhancedNnue | 4.0-5.0x | 最深 | ~194MB |

## コンパイル時最適化の利点

1. **ゼロコスト抽象化**
   - 使用しない機能のコードは生成されない
   - 例：`USE_PRUNING=false`の場合、枝刈りのコードは完全に除去

2. **インライン化**
   - const genericsにより、条件分岐が最適化される
   - 実行時の`if`文が不要

3. **型安全性**
   - 各設定の組み合わせが異なる型として表現される
   - コンパイル時に設定ミスを検出

## 使用例

```rust
use engine_core::{Engine, EngineType};

// 最強設定のエンジンを作成
let mut engine = Engine::new(EngineType::EnhancedNnue);

// NNUEの重みファイルを読み込み
engine.load_nnue_weights("path/to/weights.nnue")?;

// 探索を実行
let result = engine.search(&mut position, limits);
```

## 推奨される使用方法

- **開発・デバッグ**: `Material`エンジンを使用
- **高速な解析**: `Nnue`エンジンを使用
- **メモリ制限環境**: `Enhanced`エンジンを使用
- **最高の棋力**: `EnhancedNnue`エンジンを使用

## まとめ

統合検索フレームワークにより、4つの異なるエンジンタイプが単一のコードベースから生成されます。const genericsを活用することで、実行時オーバーヘッドなしに、各用途に最適化されたエンジンを提供できます。