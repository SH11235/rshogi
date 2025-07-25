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

## パフォーマンスベンチマーク

### バッファリング性能測定

エンジンの出力バッファリング性能を測定するベンチマークです：

```bash
# buffered-ioフィーチャーを有効にしてベンチマークを実行（推奨）
cargo bench --bench buffering_benchmark --features buffered-io

# フィーチャーなしで実行（警告が表示されます）
cargo bench --bench buffering_benchmark
```

**重要**: `--features buffered-io`なしでベンチマークを実行すると、バッファリングの効果が見られない可能性があります。

ベンチマークには以下の環境変数が設定されます：
- `USI_BENCH_MODE=1` - 0msフラッシュ遅延を許可（即時フラッシュのテスト用）

### ベンチマーク結果の見方

Criterionによる詳細なレポートが`target/criterion/`に生成されます：
- HTMLレポート: `target/criterion/buffered_io/report/index.html`
- 統計データ: `target/criterion/buffered_io/*/base/estimates.json`

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

### Byoyomi Time Control 拡張

このエンジンは複数の秒読み期間（periods）をサポートしています。以下の2つの方法で設定可能です：

1. **SetOption方式**（推奨、GUI互換性が高い）:
   ```
   setoption name ByoyomiPeriods value 3
   go btime 300000 wtime 300000 byoyomi 30000
   ```

2. **直接periodsパラメータ指定**（非標準）:
   ```
   go btime 300000 wtime 300000 byoyomi 30000 periods 3
   ```

両方が指定された場合、periodsパラメータが優先されます。未指定の場合のデフォルトは1期間です。

### サポートされるオプション

- `USI_Hash` - ハッシュテーブルサイズ (MB)
- `Threads` - 使用スレッド数
- `USI_Ponder` - ポンダー（相手の手番での思考）有効/無効
- `EngineType` - エンジンタイプ (Material/Nnue/Enhanced/EnhancedNnue)
- `ByoyomiPeriods` - 秒読み期間数 (1-10、デフォルト: 1)

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