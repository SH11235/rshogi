# Engine Core

将棋エンジンのコア実装を提供するRustライブラリです。

## 概要

engine-coreは、高性能な将棋エンジンの基盤となる機能を提供します。統一検索フレームワーク（UnifiedSearcher）により、コンパイル時の設定で様々な探索戦略を実現できます。

## 主な機能

- **統一検索フレームワーク**: const genericsを使用したゼロコスト抽象化
- **複数の探索戦略**: 基本的なアルファベータ探索から高度な枝刈り技術まで
- **評価関数**: Material評価とNNUE評価をサポート
- **時間管理**: 柔軟な時間制御と思考時間管理
- **USIプロトコル**: 標準的な将棋エンジンインターフェース

## アーキテクチャ

### UnifiedSearcher

const genericsを使用して、実行時オーバーヘッドなしに異なる探索設定を実現：

```rust
pub struct UnifiedSearcher<
    E,                          // 評価関数の型
    const USE_TT: bool,         // 置換表を使用するか
    const USE_PRUNING: bool,    // 枝刈りを使用するか
    const TT_SIZE_MB: usize,    // 置換表のサイズ（MB）
>
```

### 探索設定の例

```rust
// 基本的な探索（置換表のみ）
type BasicConfig = UnifiedSearcher<MaterialEvaluator, true, false, 8>;

// 高度な探索（置換表 + 枝刈り）
type EnhancedConfig = UnifiedSearcher<NnueEvaluator, true, true, 16>;
```

## 使用方法

### 基本的な使用例

```rust
use engine_core::{
    Position,
    search::{unified::UnifiedSearcher, SearchLimitsBuilder},
    evaluation::evaluate::MaterialEvaluator,
};
use std::sync::Arc;

// 評価関数の作成
let evaluator = Arc::new(MaterialEvaluator);

// 探索エンジンの作成
let mut searcher = UnifiedSearcher::<_, true, true, 16>::new(evaluator);

// 局面の作成
let mut position = Position::startpos();

// 探索条件の設定
let limits = SearchLimitsBuilder::default()
    .depth(10)              // 深さ10まで探索
    .fixed_time_ms(1000)    // または1秒間探索
    .build();

// 探索の実行
let result = searcher.search(&mut position, limits);

if let Some(best_move) = result.best_move {
    println!("最善手: {}", best_move);
    println!("評価値: {}", result.score);
}
```

### 探索パラメータ

`SearchLimitsBuilder`で以下のパラメータを設定可能：

- `depth(u8)`: 最大探索深さ
- `fixed_time_ms(u64)`: 固定思考時間（ミリ秒）
- `nodes(u64)`: 最大探索ノード数
- `stop_flag(Arc<AtomicBool>)`: 外部からの停止フラグ
- `ponder_hit_flag(Arc<AtomicBool>)`: ポンダーヒットフラグ

## モジュール構成

- `shogi/`: 将棋のルールと局面管理
  - `board.rs`: 盤面表現
  - `moves.rs`: 手の生成と検証
  - `position.rs`: 局面管理
  
- `search/`: 探索アルゴリズム
  - `unified/`: 統一検索フレームワーク
  - `limits.rs`: 探索制限の設定
  - `tt.rs`: 置換表の実装
  
- `evaluation/`: 評価関数
  - `material.rs`: 駒の価値による評価
  - `nnue/`: ニューラルネットワーク評価
  
- `time_management/`: 時間管理
  - 持ち時間と秒読みの管理
  - 最適な思考時間の計算

## パフォーマンス

Phase 4のベンチマーク結果（開始局面、深さ4）：

| 設定 | 実行時間 | 相対性能 |
|---|---:|---:|
| 基本設定（枝刈りなし） | 139ms | 1.0x |
| 拡張設定（枝刈りあり） | 7.3ms | 19.0x |

const genericsによりゼロコスト抽象化を実現し、実行時オーバーヘッドはありません。

## ビルドとテスト

```bash
# ビルド
cargo build --release

# テスト
cargo test

# ベンチマーク
cargo bench --bench search_benchmarks
```

## ライセンス

このプロジェクトのライセンスについては、リポジトリのルートにあるLICENSEファイルを参照してください。