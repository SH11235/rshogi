# 統合テストノート

## 概要

USIエンジンには3種類の統合テストが実装されています：

### 1. フル統合テスト (`integration_test.rs`)
実際のエンジンプロセスを起動して実世界の動作をテストします：
- **test_stop_response_time**: stopコマンドが500ms以内にbestmoveを返すことを検証
- **test_quit_clean_exit**: メモリリークなくクリーンに終了することを確認
- **test_stop_during_deep_search**: 深い探索の中断をテスト
- **test_multiple_stop_commands**: 複数の探索/停止サイクルを検証

**注意**: これらのテストはエンジンバイナリのビルドが必要で、実行に時間がかかります。CI/CD環境に適しています。

### 2. ユニット統合テスト (`integration_test_simple.rs`)
プロセスを起動せずに重要な動作を検証する軽量テスト：
- **test_engine_compiles_and_runs**: 基本的なコンパイルチェック
- **test_search_info_formatting**: depth 0が表示されないことを検証
- **test_time_minimum_value**: 時間が常に最低1msであることを確認

### 3. Info出力テスト (`info_output_test.rs`)
探索中のinfo出力動作を検証：
- **test_info_output_during_search**: 各深さでinfo出力が行われることを確認
- **test_info_output_with_early_stop**: stop_flag設定時の動作を検証

## テストされる重要な動作

### Stop応答時間
- GUI要件：stopコマンド後500ms以内にbestmoveを送信すること
- GUIタイムアウトを防ぎ、レスポンシブなユーザー体験を保証
- Acquire/Release順序付きのatomic booleanフラグで実装

### クリーンな終了
- すべてのスレッドが適切にjoinされることを確認
- 終了前にワーカーキューがドレインされる
- アクティブな探索中のquit時にリソースリークがない

### エラーハンドリング
- エラーメッセージは`info string Error:`経由でGUIに適切に転送される
- シャットダウン時にメッセージが失われない

### Info出力のリアルタイム性
- 各反復深化の深さ完了時にinfo出力
- 探索の進行状況をGUIにリアルタイムで通知
- stop_flag設定後も適切にinfo出力

## テストの実行

```bash
# 簡易統合テストの実行（高速）
cargo test -p engine-cli --test integration_test_simple

# Info出力テストの実行
cargo test -p engine-cli --test info_output_test

# フル統合テストの実行（ビルド済みバイナリが必要）
cargo test -p engine-cli --test integration_test

# 特定のテストの実行
cargo test -p engine-cli test_stop_response_time

# すべての統合テストを実行
cargo test -p engine-cli --tests
```

## 実装の詳細

1. **アトミックストップフラグ**: スレッド間で共有され、即座に探索を中断
2. **メッセージドレイン**: `flush_worker_queue`がメッセージの喪失を防ぐ
3. **スレッドジョイン**: すべてのワーカースレッドが終了時に適切にjoinされる
4. **時間保証**: 最小1msの時間報告によりGUIの混乱を防ぐ
5. **Info出力コールバック**: 探索エンジンからのリアルタイム進行状況報告