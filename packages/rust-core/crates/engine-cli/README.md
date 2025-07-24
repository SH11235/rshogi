# USI Engine CLI

USI (Universal Shogi Interface) プロトコルに準拠した将棋エンジンのコマンドラインインターフェース実装です。

## 概要

このクレートは、engine-coreの探索エンジンをUSIプロトコル経由で利用可能にするアダプタを提供します。

## 主な機能

- USIプロトコルの完全実装
- マルチスレッド対応（ワーカースレッドによる非同期探索）
- リアルタイムinfo出力
- 適切なエラーハンドリングとクリーンなシャットダウン

## ビルドと実行

```bash
# ビルド
cargo build -p engine-cli --release

# 実行
cargo run -p engine-cli

# または直接バイナリを実行
./target/release/engine-cli
```

## テスト

### ユニットテスト
```bash
cargo test -p engine-cli --lib
```

### 統合テスト
```bash
# 軽量テスト（高速）
cargo test -p engine-cli --test integration_test_simple

# Info出力テスト
cargo test -p engine-cli --test info_output_test

# フル統合テスト（要ビルド済みバイナリ）
cargo test -p engine-cli --test integration_test

# すべてのテスト
cargo test -p engine-cli
```

詳細は[統合テストのドキュメント](docs/integration-testing-notes.md)を参照してください。

## USIコマンド

サポートされているUSIコマンド：
- `usi` - エンジン情報を返す
- `isready` - 初期化完了を確認
- `setoption` - オプション設定
- `position` - 局面設定
- `go` - 探索開始
- `stop` - 探索停止
- `ponderhit` - ポンダーヒット
- `gameover` - ゲーム終了通知
- `quit` - エンジン終了

## アーキテクチャ

- **メインスレッド**: USI I/O処理
- **ワーカースレッド**: 探索実行
- **チャンネル通信**: スレッド間の非同期メッセージング
- **アトミックフラグ**: 探索の即座中断

## 開発者向け情報

- `src/main.rs` - メインループとスレッド管理
- `src/engine_adapter.rs` - engine-coreとのブリッジ
- `src/usi/` - USIプロトコル実装
  - `parser.rs` - コマンドパーサ
  - `commands.rs` - コマンド定義
  - `output.rs` - レスポンスフォーマット
  - `conversion.rs` - Move/SFEN変換