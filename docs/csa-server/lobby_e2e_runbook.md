# Workers ロビー対局の実行手順

`csa_client` を 2 端末で起動し、相手待ちから対局・棋譜保存までを確認する手順。
ロビーの設定と通信は [ロビーの動作](lobby_design.md)、Worker の構築は
[デプロイ手順](deployment.md) を参照。

## 1. 準備

リポジトリ直下でビルドする。エンジンに使うモデルの仕様に応じたビルドは
[ビルドガイド](../build.md) を参照。

```bash
cargo build --release -p rshogi-csa-client -p rshogi-usi
curl --fail https://stg.rshogi-csa-server.sh11235.com/health
```

`/health` が成功することを確認する。モデルは `$SHOGI_DATA/nnue/` に用意し、
以下の `EvalFile` を実際のモデル名へ差し替える。Windows はバイナリに `.exe` を付ける。

## 2. 2 端末で対局する

各コマンドはリポジトリ直下から実行する。両端末で同じ `game-name`、異なるハンドル、
反対の手番を指定する。ハンドルに認証を設定した環境では `--password` も指定する。

```bash
# 端末 A（先手）
./target/release/csa_client \
  --target staging --lobby --game-name byoyomi-120-5 \
  --handle alice --color black \
  --engine ./target/release/rshogi-usi \
  --options "EvalFile=$SHOGI_DATA/nnue/your-model.bin,USI_Hash=256" \
  --max-games 1 --record-dir ./records/alice --log-level info
```

```bash
# 端末 B（後手）
./target/release/csa_client \
  --target staging --lobby --game-name byoyomi-120-5 \
  --handle bob --color white \
  --engine ./target/release/rshogi-usi \
  --options "EvalFile=$SHOGI_DATA/nnue/your-model.bin,USI_Hash=256" \
  --max-games 1 --record-dir ./records/bob --log-level info
```

`--game-name` は `CLOCK_PRESETS` に登録された名前を使う。例の `byoyomi-120-5` は
2 分 + 5 秒読み。`byoyomi-msec-10-100` は 10 秒 + 100ms 秒読みであり、通信遅延だけでも
時間切れになりやすい。サーバーのグローバル時計より指定プリセットが優先される。

`--target production` で本番の内蔵接続先へ切り替えられる。自前 Worker へ接続する場合の
TOML / `--host` 指定は [CSA クライアント](../csa-client.md) を参照。

## 3. 進行・停止・継続

| ログの目印 | 確認できること |
|---|---|
| `LOGIN_LOBBY OK` | 待機キューへ登録された |
| `MATCHED 受信` | 対局相手とルームが決まった |
| `対局情報受信` → `対局開始` | Game_Summary を受信し、AGREE / START を終えた |
| `対局 #1 結果` → `棋譜保存`（順序は経路による） | 結果とローカル出力を確認できる |

詳細を追う場合は `--log-level debug`。CSA 送信ログのパスワードと再接続トークンはマスクする。
Workers のサーバーログは次のコマンドで確認する。

```bash
cd crates/rshogi-csa-server-workers
pnpm exec wrangler tail --config wrangler.staging.toml --format pretty
```

`--max-games 5` なら毎局ロビーへ戻って 5 局、`0` なら無制限に対局する。
Ctrl-C は待機・対局を中止する。再開は同じコマンドを再実行する。対局中の自動再接続は
サーバーの `Reconnect_Token` と猶予設定がある場合だけ働き、プロセス再起動後の対局復帰は行わない。

## 4. 棋譜を確認する

`--record-dir` の下に CSA / SFEN、既定ではその `jsonl/` に 1 局ごとの JSONL を保存する。
ファイル名の日時はクライアントのローカル時刻、CSA の開始・終了時刻は JST。
評価コメントは自エンジンの手にだけ入り、JSONL にはミリ秒の時間や探索情報も含む。
書式と終局理由は [記録リファレンス](../csa-client-records.md) を参照。

サーバー確定棋譜は R2 の `KIFU_BUCKET` に `YYYY/MM/DD/<game_id>.csa` として保存する。
日付は対局開始時刻に基づく。サーバーの `BEGIN Time` と指し手の `T` はその棋譜の
`Time_Unit` に合わせて読む。クライアントの CSA V2 は秒単位なので、時間表記の単純な文字列一致では比較しない。

```bash
# Worker crate 内で実行。bucket とキーは設定・ログの値へ差し替える。
pnpm exec wrangler r2 object get \
  'rshogi-csa-kifu-staging/YYYY/MM/DD/<game_id>.csa' \
  --remote --config wrangler.staging.toml --file server.csa
```

`ALLOW_FLOODGATE_FEATURES=true` の環境は `FLOODGATE_HISTORY_BUCKET` に履歴 JSON も保存する。
キーは `floodgate-history/YYYY/MM/DD/HHMMSS-<game_id>.json`。履歴の `result_code` は
`#RESIGN` / `#TIME_UP` などの終局理由を表し、勝者は `winner` で確認する。
`#WIN` / `#LOSE` は対局者向けの通知であり、履歴の理由コードではない。

## 5. 困ったとき

| 症状 | 確認・対処 |
|---|---|
| `MATCHED` が届かない | 相手の接続、同じ game_name、反対の色を確認 |
| `bad_game_name` / `unknown_game_name` | ASCII 英数字・`-`・`_` の 1〜32 文字、登録済みプリセット名を使う |
| `handle_auth_failed` | 登録済みハンドルのパスワードと認証設定を確認 |
| `queue_full` | 待機上限または相手の滞留状況を確認して再試行 |
| `queue_expired` | 生存確認間隔をサーバーの TTL より短くする。`--keep-alive 0` は周期送信を無効化する |
| HTTP 403 | 明示した Origin と allowlist を確認。ネイティブ利用では通常 `--ws-origin` は不要 |
| HTTP 429 / 503、`rate_limited` | `Retry-After` / `retry_after` が指定する時間だけ待つ |
| `MATCHED` 後に進まない | 相手の GameRoom 接続と `wrangler tail` を確認。必要なら Ctrl-C で中止して再接続 |
| 時間切れが多い | 指定プリセットと通信マージンを確認。短い秒読みでは [時間管理](../csa-client.md) を見直す |
| ローカル棋譜がない | `record.enabled` / `save_csa` / `save_jsonl` と保存先の警告ログを確認 |
| R2 棋譜・履歴がない | 終局・書き出しログ、R2 binding と履歴ゲートを確認。失敗時の再試行は [デプロイ運用](deployment.md) を参照 |
