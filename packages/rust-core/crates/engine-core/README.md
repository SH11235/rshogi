# Engine Core

Rust 製将棋エンジンのコアライブラリです。`ClassicBackend` を中心に、探索アルゴリズム・評価関数・USI インタフェースに必要なコンポーネントを提供します。

## 主な特徴

- **ClassicBackend**: 反復深化 + PVS + 各種枝刈り（Null Move / LMR / Razor / ProbCut / IID / Static Beta Pruning）を備えた単スレ探索器。
- **SearchProfile / SearchParams**: EngineType（Material / Enhanced / Nnue / EnhancedNnue）ごとに既定設定を切り替えつつ、USI `setoption` でランタイム調整が可能。
- **評価関数**: 駒割り評価 (`MaterialEvaluator`) と NNUE 評価 (`NNUEEvaluatorWrapper`) をサポート。
- **時間管理と停止制御**: `SearchLimits` + `StopController` により、USI 側の締切（panic/hard）と探索内部の判定を統合。
- **補助モジュール**: TT（16B エントリ）、Move Ordering（History/Killer/Counter）、Aspiration などをモジュール分割して実装。

## 代表的なモジュール

- `search/ab/` … ClassicBackend 本体 (`driver.rs`, `pvs.rs`, `qsearch.rs`, `pv_extract.rs`, `ordering/`, `pruning/`)
- `search/api.rs` … `SearcherBackend` トレイトと InfoEvent ブリッジ
- `search/params.rs` … 探索パラメータ（LMR/LMP/ProbCut 等）の集中管理
- `search/tt/` … 置換表実装
- `evaluation/` … `MaterialEvaluator` と `NNUEEvaluatorWrapper`
- `time_management/` … 思考時間計算と締切制御

## 使い方（シンプルな例）

```rust
use std::sync::Arc;
use engine_core::{
    engine::controller::{Engine, EngineType},
    search::{SearchLimits, SearchLimitsBuilder},
    Position,
};

fn main() {
    // EngineType::EnhancedNnue を選択（ClassicBackend + NNUE + Enhanced プロファイル）
    let mut engine = Engine::new(EngineType::EnhancedNnue);

    // 開始局面を用意
    let mut pos = Position::startpos();

    // 探索条件（深さ 6）を構築
    let limits = SearchLimitsBuilder::default().depth(6).build();

    // 同期的に探索実行
    let result = engine.think_blocking(&pos, &limits, None);
    if let Some(best) = result.best_move {
        println!("bestmove {}", best);
    }
}
```

USI アプリケーションからは `engine-usi` クレートを利用することで、`setoption` → `go` → `bestmove` の一連の操作を行えます。

## エンジンタイプとプロファイル

EngineType と SearchProfile の対応は次のとおりです（詳細は [docs/engine-types-guide.md](../../docs/engine-types-guide.md) を参照）。

| EngineType     | Evaluator           | SearchProfile                | 用途               |
|----------------|---------------------|------------------------------|--------------------|
| Material       | MaterialEvaluator   | `basic_material()`           | デバッグ・学習     |
| Enhanced       | MaterialEvaluator   | `enhanced_material()`        | 省メモリ/長考      |
| Nnue           | NNUEEvaluatorProxy  | `basic_nnue()`               | 高速検討           |
| EnhancedNnue   | NNUEEvaluatorProxy  | `enhanced_nnue()`            | 対局・最強設定     |

EngineType を切り替えると、対応する `SearchProfile` が `SearchParams` の既定値を初期化します。個別調整が必要な場合は、USI `setoption name SearchParams.*` で再設定してください。

## 開発・テスト

```bash
# ビルド
cargo build --release

# 単体テスト
cargo test

# 代表的な診断 CLI（例）
cargo run --release --example classicab_diagnostics -- --depth-min 8 --depth-max 8 --time-ms 10000
```

### StopController に関連するオプション

- セッション ID 不一致時に `debug_assert!` で即座に検知したい場合は `engine-core` を `--features strict-stop-session-assert` 付きでビルドします。CI での再現確認は `cargo test -p engine-core --features strict-stop-session-assert` などで実施できます。
- リードウィンドウ停止時に `Finalize(Planned)` を送出する挙動は既定で有効です。互換性検証などで従来動作に戻したい場合は `SHOGI_LEAD_WINDOW_FINALIZE=off`（`0` / `false` も可）を環境変数に設定してください。

## ライセンス

リポジトリルートの `LICENSE` を参照してください。
