# Workers ロビーの動作と設定

ロビー `/ws/lobby` は、同じ時計プリセット・反対の手番で待つ 2 人を組み合わせる。
成立すると両者へ `MATCHED <room_id> <color>` を送り、各クライアントは
`/ws/<room_id>` へ接続し直して通常の CSA 対局を始める。

起動例と確認手順は [ロビー対局の実行手順](lobby_e2e_runbook.md)、
直接ルームへの接続は [CSA クライアント](../csa-client.md) を参照。

## 接続とマッチング

`csa_client` は `--target staging|production --lobby --game-name <preset>` に
`--handle`、`--color black|white`、`--engine` を指定する。`--room-id` は不要。
相手と同じ `--game-name`、互いに反対の `--color` を使う。

- `game_name` は ASCII 英数字・`-`・`_` の 1〜32 文字。
- `CLOCK_PRESETS` が 1 件以上あれば、登録済みの `game_name` だけを受理する。
  空配列の環境では任意の名前にグローバル時計を適用する。
- 同じハンドルの新規ログインは古い待機接続を置き換える。
- 対局後は再びロビーへ接続し、`--max-games`（`0` は無制限）まで繰り返す。
- Ctrl-C は待機・対局を中止する。対局中の再接続は GameRoom が管理し、ロビーの
  待機キューには戻さない。再接続できなければ次の対局の待機へ進む。

## 通信

WebSocket のテキストメッセージで 1 行ずつ送受信する。

| 方向 | 行 | 意味 |
|---|---|---|
| client → server | `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` | 公開対局の待機登録。手番は `black` / `white`（別名 `sente` / `gote` も受理） |
| client → server | `LOGOUT_LOBBY` | 待機から離脱 |
| client → server | `LOBBY_PONG` | 生存確認時刻を更新 |
| server → client | `LOGIN_LOBBY:<handle> OK` | 待機登録成功 |
| server → client | `LOGIN_LOBBY:incorrect <reason>` | 登録拒否または待機期限切れ |
| server → client | `MATCHED <room_id> <color>` | 対局成立。割当先へ接続し直す |

`room_id` は `lobby-<game_name>-<32桁hex>`。`MATCHED` の後にロビー接続は閉じられる。
現サーバーは `LOBBY_PING` を送信しない。`csa_client` は待機中、
`server.keepalive.ping_interval_sec`（既定 30 秒）ごとに `LOBBY_PONG` を送る。
`0` にすると周期送信を止めるため、相手待ちが長いと期限切れになる。

主な拒否理由は `bad_format` / `bad_id_format` / `bad_color` / `bad_game_name` /
`unknown_game_name` / `queue_full` / `queue_expired` / `handle_auth_failed` /
`rate_limited retry_after=<秒>`。レート制限時はサーバー指定の待ち時間を尊重する。
接続前の HTTP 429 / 503 も `Retry-After` に従って再試行する。

## 認証・接続制限

`password` は省略できない。`WORKERS_HANDLE_AUTH` に登録したハンドルはパスワードを検証し、
未登録のハンドルは任意の非空パスワードを使える。`csa_client --target` では未指定時に
`anything` を補う。本人確認の設定は [管理認証](admin_auth.md) を参照。

Origin の検査は直接ルームと共通。ネイティブクライアントは通常 Origin を送らない。
ブラウザーや明示した `--ws-origin` は allowlist に合わせる。
[観戦・接続のアクセス制御](viewer_access_control.md) も参照。

## キューの上限と期限

| 設定 | 未設定時 | 動作 |
|---|---|---|
| `LOBBY_QUEUE_SIZE_LIMIT` | `100` | 全 game_name を合わせた公開待機人数の上限 |
| `LOBBY_QUEUE_ENTRY_TTL_SEC` | `300` 秒 | 最後の登録・PONG からの有効期間。到達時は `queue_expired` を通知して切断 |

環境別の値は `wrangler.staging.toml` / `wrangler.production.toml` を確認する。
生存確認の送信間隔は TTL より短くする。接続は WebSocket attachment に待機状態と
最終生存確認時刻を保存し、Durable Object の休止復帰後も待機を続ける。

新規接続は 30 秒以内にログインする必要がある。未認証接続には全体 100・IP ごと 4 の
上限もある。ログインのレート制限は [レート制限](rate_limit.md) を参照。

対局ルームに片方だけが接続した場合、公開ルームに専用の handoff timeout はない。
両者の LOGIN 後に `Game_Summary` を送り、そこから `AGREE_TIMEOUT_SECONDS`
（未設定時 60 秒）で AGREE 待ちを管理する。`MATCHED` 後に進まなければ相手の接続と
サーバーログを確認し、必要なら Ctrl-C で中止して再接続する。

## 私的対局

`PRIVATE_CHALLENGE_ENABLED=true` の環境では、招待トークンと `free` による私的対局も
利用できる。公開キューとは別の手順なので、[私的対局のプロトコル](protocol-reference.md#921-workers-の私的対局)
を参照。`csa_client --color` が受理する値は `black` / `white`。
