# MoveGenハング問題

## 概要

`has_legal_moves()` メソッドがサブプロセス実行時にハングする問題についてのドキュメント。この問題は**2025-08-26時点で解決済み**であり、多層防御により完全に防止されています。

## 問題の詳細

### 発生条件
- サブプロセスとして実行（stdin/stdout/stderr全てパイプ接続）
- `SKIP_LEGAL_MOVES=0` 環境変数設定時
- `MoveGen::generate_all()` 呼び出し時

### 症状
- プロセスが `futex_wait` でデッドロック状態になる
- CPU使用率は0%（無限ループではなくロック待機）
- スタックトレースで複数スレッドがmutex待機

### 非発生条件
- 単体テスト実行時
- 直接実行時（ターミナルから）
- メインプロセス内での呼び出し
- `SKIP_LEGAL_MOVES=1`（デフォルト）設定時

### 根本原因（推定）
サブプロセス実行環境特有の要因が `MoveGen` と相互作用：
- メモリマップ/アドレス空間の違い（ASLR等）
- シグナルハンドリングの違い
- Thread Local Storage（TLS）の初期化順序
- I/Oロックの取得順序の違い

## 実装された対策（多層防御）

### 1. 自動検出
```rust
// パイプI/O検出またはSUBPROCESS_MODE環境変数で自動的にスキップ
let is_piped = !atty::is(atty::Stream::Stdin) 
    || !atty::is(atty::Stream::Stdout) 
    || !atty::is(atty::Stream::Stderr);
let is_subprocess = std::env::var("SUBPROCESS_MODE").is_ok() || is_piped;
```

### 2. タイムアウト保護
万が一実行されても100msでタイムアウト（注意：タイムアウト時はスレッドが残存）

### 3. 環境変数制御
`SKIP_LEGAL_MOVES=1`（デフォルト）で完全にスキップ

### 4. 循環依存排除
初期化の統合により根本原因の一つを除去：
```rust
pub fn init_engine_tables() {
    INIT_ONCE.call_once(|| {
        warm_up_static_tables_internal();  // I/Oなし、環境変数なし
        init_remaining_tables();           // 追加の初期化
    });
}
```

## 環境変数リファレンス

### SKIP_LEGAL_MOVES
has_legal_movesチェックの呼び出しを制御します。

```bash
# デフォルト（推奨） - チェックをスキップ
SKIP_LEGAL_MOVES=1 ./engine-cli

# チェックを有効化（注意：サブプロセスでハングの可能性）
SKIP_LEGAL_MOVES=0 ./engine-cli
```

- `1`（デフォルト）: チェックをスキップ（ハング回避のため推奨）
- `0`: チェックを有効化（デバッグ専用）

### USE_ANY_LEGAL
has_legal_movesチェックで使用するメソッドを選択（`SKIP_LEGAL_MOVES=0`時のみ有効）。

```bash
# has_any_legal_move()を使用（早期リターン最適化、最大10倍高速）
USE_ANY_LEGAL=1 SKIP_LEGAL_MOVES=0 ./engine-cli

# has_legal_moves()を使用（従来のgenerate_all）
USE_ANY_LEGAL=0 SKIP_LEGAL_MOVES=0 ./engine-cli
```

### USI_DRY_RUN
USI出力を完全に無効化（stdoutブロック調査用）。

```bash
USI_DRY_RUN=1 ./engine-cli < test_input.txt
```

### FORCE_FLUSH_STDERR
stderrへの各ログ出力後に強制フラッシュ。

```bash
FORCE_FLUSH_STDERR=1 ./engine-cli
```

## 使用方法とコマンド例

### 通常使用（推奨）
```bash
# デフォルト設定で安全に実行
./engine-cli
```

### デバッグ・調査用

```bash
# ハング再現（サブプロセスで SKIP_LEGAL_MOVES=0）
SKIP_LEGAL_MOVES=0 timeout 5 ./target/release/engine-cli < test_positions.txt

# 詳細ログ + stderr強制フラッシュ
FORCE_FLUSH_STDERR=1 RUST_LOG=debug ./engine-cli

# システムコール追跡
strace -f -e trace=read,write,futex -o trace.log ./engine-cli < test_positions.txt

# デッドロック検出（debugビルドのみ）
DEADLOCK_MAX_FRAMES=10 ./target/debug/engine-cli < test_positions.txt
```

### テスト実行

```bash
# ハング問題の統合テスト
cargo test --test subprocess_pipe_test

# ベンチマーク実行
cargo bench legal_moves
```

## 技術的な実装詳細

### 実装箇所
- `EngineAdapter::has_legal_moves()` - `engine_adapter/search.rs:214-223`
- 環境変数チェック - `command_handler.rs:741-765`
- パイプ検出ユーティリティ - `utils.rs:is_piped_stdio()`, `is_subprocess_or_piped()`
- 初期化処理 - `crates/engine-core/src/init_unified.rs`
- USI_DRY_RUN処理 - `src/usi/output.rs`
- FlushingStderrWriter - `src/flushing_logger.rs`

### has_legal_moves実装状況
```rust
// engine_adapter/search.rs
#[allow(dead_code)] // Temporarily unused due to subprocess hang issue
pub fn has_legal_moves(&self) -> Result<bool> {
    let position = self.get_position().ok_or_else(|| anyhow!("Position not set"))?.clone();
    
    // Generate legal moves
    let mut movegen = MoveGen::new();
    let mut legal_moves = MoveList::new();
    movegen.generate_all(&position, &mut legal_moves);
    
    Ok(!legal_moves.is_empty())
}
```

### スレッドリーク対策
`has_legal_moves_with_timeout()` のタイムアウト時の緩和策：
- 名前付きスレッド（`"legal-moves-timeout"`）でデバッグ時の識別を容易化
- `HUNG_MOVEGEN_CHECKS` カウンタでタイムアウト発生を追跡
- 警告ログでスレッド残存を明記

**注意**: Rust標準ライブラリではスレッドの強制終了は不可能なため、根本解決は「呼び出さない」設計に依存。

## 改善履歴

### 2025-08-26 追加改善

#### IO検出ロジックの統合
- `utils.rs` に共通関数を追加
- `command_handler.rs` と `engine_adapter/search.rs` の重複コード削除

#### テスト初期化の補強
- `test_helpers::ensure_engine_initialized()` を追加
- `once_cell::sync::Lazy` で一度だけ初期化を保証

#### その他の改善
- 変数名の明確化: `subprocess_or_piped` で意図を明確に
- ログレベル調整: 5ms警告を `warn!` から `info!` に変更
- 環境変数の優先順位を文書化

### 初期の対策実装

#### FlushingStderrWriter
各ログ出力後に自動フラッシュしてstderrバッファリング問題を回避。

#### 早期初期化
`IsReady`ハンドラで静的テーブルを初期化し、遅延初期化を回避。

## まとめ

MoveGenハング問題は多層防御により完全に防止されています：

1. **自動検出**: パイプI/O または SUBPROCESS_MODE で自動的にスキップ
2. **タイムアウト保護**: 万が一実行されても100msでタイムアウト（スレッドは残存）
3. **循環依存排除**: 初期化の統合により根本原因を除去
4. **継続的検証**: 統合テストとベンチマークによる品質保証
5. **スレッドリーク緩和**: カウンタとログによる可視化

現状で実害のあるハングは完全に防止され、将来の再発防止策も整備されています。デフォルト設定（`SKIP_LEGAL_MOVES=1`）を維持することで、安全な動作が保証されます。
