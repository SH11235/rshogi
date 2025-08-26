# Debug Environment Variable Controls

## 概要

エンジンのデバッグと問題調査を容易にするための環境変数制御機能。

**注意**: 当初「MoveGenハング問題」として調査されていましたが、詳細な調査の結果、has_legal_movesチェック自体が未実装であることが判明しました。これらの環境変数は一般的なデバッグ目的には有用です。

## 環境変数

### 1. USI_DRY_RUN

USI出力を完全に無効化します。stdoutブロックが原因かどうかを即座に判定できます。

```bash
# USI出力を無効化（デバッグ用）
USI_DRY_RUN=1 ./engine-cli < test_input.txt
```

- `1`: すべてのUSI出力（send_response, send_info_string）をスキップ
- その他/未設定: 通常動作

### 2. SKIP_LEGAL_MOVES (レガシー)

**注意**: has_legal_movesチェックは実際には実装されていないため、この環境変数は現在効果がありません。将来の互換性のために残されています。

```bash
# レガシー設定（効果なし）
SKIP_LEGAL_MOVES=1 ./engine-cli
```

- `1`: デバッグメッセージを出力（チェックは未実装）
- `0`: デバッグメッセージを出力（チェックは未実装）
- その他/未設定: `1`と同じ

### 3. FORCE_FLUSH_STDERR

stderrへの各ログ出力後に強制フラッシュを行います。

```bash
# stderr自動フラッシュを有効化
FORCE_FLUSH_STDERR=1 ./engine-cli
```

- `1`: FlushingStderrWriterを使用（各出力後にflush）
- その他/未設定: 通常のstderr出力（バッファリングあり）

## 使用例

### 本番運用（現状のワークアラウンド維持）
```bash
SKIP_LEGAL_MOVES=1 ./engine-cli
```

### stdoutブロック調査
```bash
# USI出力を無効化してハングが消えるか確認
USI_DRY_RUN=1 ./engine-cli < test_input.txt

# ハングが消えた場合 → stdout詰まりが原因
# ハングが継続する場合 → 他の要因
```

### デバッグモード
```bash
# 詳細ログ + stderr強制フラッシュ
FORCE_FLUSH_STDERR=1 RUST_LOG=debug ./engine-cli
```

### 完全な調査モード
```bash
# すべての機能を有効化
SKIP_LEGAL_MOVES=1 FORCE_FLUSH_STDERR=1 RUST_LOG=trace ./engine-cli
```

## 実装詳細

### USI_DRY_RUN
- `src/usi/output.rs`の`send_response()`関数の先頭でチェック
- 設定時はすべてのUSI出力を`Ok(())`で即座に返す
- ログ出力（stderr）は影響を受けない

### SKIP_LEGAL_MOVES
- `src/command_handler.rs`のGoコマンドハンドラ内でチェック
- **実際にはhas_legal_moves()メソッドは存在しないため、効果なし**
- デバッグメッセージの出力レベルを変えるだけ

### FORCE_FLUSH_STDERR
- `src/main.rs`のロガー初期化時にチェック
- 設定時は`FlushingStderrWriter`を使用
- 通常時は標準の`Target::Stderr`を使用

## 調査手順

1. **基本確認**
   ```bash
   # 通常実行でハング確認
   ./engine-cli < test_input.txt
   ```

2. **stdout詰まり確認**
   ```bash
   # USI出力無効化
   USI_DRY_RUN=1 ./engine-cli < test_input.txt
   ```

3. **has_legal_movesスキップ確認**
   ```bash
   # ワークアラウンド有効化
   SKIP_LEGAL_MOVES=1 ./engine-cli < test_input.txt
   ```

4. **システムコール追跡**
   ```bash
   strace -f -e trace=read,write,futex -o trace.log ./engine-cli < test_input.txt
   ```

これらの環境変数により、問題の切り分けと本番運用の両立が可能になります。