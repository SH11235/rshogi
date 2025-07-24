# CIテスト設定

## 大容量スタックテスト

一部のテストはNNUE初期化のために大きなスタックサイズを必要とします。これらのテストはCI失敗を防ぐため、デフォルトで `#[ignore]` でマークされています。

### 大容量スタックが必要なテスト:
- `test_enhanced_nnue_engine`
- `test_engine_type_switching_with_enhanced_nnue`
- `test_engine_type_switching_with_nnue`
- `test_nnue_engine`
- `test_parallel_engine_execution`
- `test_concurrent_weight_loading`
- `test_info_output_during_search`
- `test_info_output_with_early_stop`
- `test_engine_type_switching_basic`
- `test_load_nnue_weights_wrong_engine_type`
- `test_enhanced_engine_with_stop_flag`
- `test_material_engine`
- `test_enhanced_engine`

注意: 現在のデバッグビルドでは、すべてのエンジンテストでスタックオーバーフローが発生する可能性があります。リリースビルドでは問題が軽減される場合があります。

### ローカルでの実行:
```bash
# 無視されたテストを含むすべてのテストを増加したスタックサイズで実行
RUST_MIN_STACK=8388608 cargo test -- --ignored

# 特定のテストを実行
RUST_MIN_STACK=8388608 cargo test test_enhanced_nnue_engine -- --ignored
```

### CI設定オプション:

#### オプション1: リリースビルドでテスト（推奨）
```yaml
# リリースビルドではスタックサイズの問題が軽減されます
- run: cargo test --release
```

#### オプション2: デバッグビルドで増加したスタックサイズ
```yaml
# デバッグビルドでのテスト（開発中）
- run: RUST_MIN_STACK=8388608 cargo test
```

#### オプション3: 段階的なテスト戦略
```yaml
# 通常のテスト（NNUE関連以外）
- run: cargo test

# NNUE関連テスト（リリースビルドまたは大容量スタック）
- run: cargo test --release -- --ignored
  # または
- run: RUST_MIN_STACK=8388608 cargo test -- --ignored
  continue-on-error: true
```

#### オプション4: プロファイル別設定
```yaml
jobs:
  test:
    strategy:
      matrix:
        profile: [debug, release]
    steps:
      - name: Run tests
        run: |
          if [ "${{ matrix.profile }}" = "debug" ]; then
            RUST_MIN_STACK=8388608 cargo test
          else
            cargo test --release
          fi
```

## フィーチャーフラグ

CI環境に基づいて条件付きでテストをコンパイルする場合:

```toml
# Cargo.toml
[features]
large-stack-tests = []
```

CIで実行:
```yaml
# 必要な場合のみ大容量スタックテストを有効化
- run: cargo test --features large-stack-tests
```