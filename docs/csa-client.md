# CSA対局クライアント (`csa_client`)

USIエンジンをCSAプロトコル対局サーバー（floodgate等）にCLIから接続するツール。
将棋所などのGUIを介さず、バックグラウンドで連続対局を実行できる。

## クイックスタート

```bash
# 1. 設定ファイルを用意（example をコピーして編集）
cp crates/rshogi-csa-client/examples/csa_client.toml.example ~/csa-client.toml
# → server.host, server.id, server.password, engine.path を書き換える

# 2. 実行
cargo run -p rshogi-csa-client --release -- ~/csa-client.toml --max-games 1
```

設定ファイルには認証情報が入るため、リポジトリ外へ保存する。
`[CSA] 対局待機中...` → `対局情報受信` → `対局開始` → `対局 #1 結果` の順で進む。
既定では `./records/` に CSA / SFEN、`./records/jsonl/` に JSONL を保存する。
Ctrl-C で待機・対局を中止する。通常運用で連続対局する場合は `--max-games 0` にする。

## 設定

TOML 設定ファイルで管理する。[コメント付き設定例](../crates/rshogi-csa-client/examples/csa_client.toml.example)は TCP / WebSocket の両経路に対応する。

### 設定の優先順位

CLI オプション > 環境変数 > `--target` プリセット > TOML 設定ファイル > デフォルト値。
`--target` は接続先・ログイン ID・Origin・floodgate モードを設定する。

### CLIオプション

設定ファイルの値を部分的にオーバーライドできる:

```bash
cargo run -p rshogi-csa-client --release -- config.toml \
  --id my_engine \
  --max-games 10 \
  --ponder true \
  --hash 2048 \
  --options "Threads=8,EvalFile=/path/to/nn.bin"
```

すべての CLI オプションを以下に示す。`—` は CLI で値を上書きせず、上記の優先順位に従う意味。
設定ファイルのパスは省略できるが、ログイン ID とエンジンパスは必要になる。

| オプション | 省略時の値 | 説明 |
|---|---|---|
| `--target staging\|production` | — | 同梱 Workers 環境の接続プリセット |
| `--room-id ID` | — | 直接ルーム接続時の ID。`--target` 使用時は必須（ロビーでは不要） |
| `--handle NAME` | — | `--target` / `--lobby` のハンドル |
| `--color black\|white` | — | `--target` / `--lobby` の手番 |
| `--lobby` | `false` | `/ws/lobby` で自動マッチングを待つ |
| `--game-name NAME` | — | 時計プリセット名。`--target` / `--lobby` 使用時は必須 |
| `--host HOST` | `wdoor.c.u-tokyo.ac.jp` | TCP ホスト名、または WebSocket URL |
| `--port PORT` | `4081` | TCP ポート。WebSocket では URL のポートを使う |
| `--id ID` | — | CSA ログイン ID |
| `--password PASSWORD` | — | パスワード。`--target` では空なら `anything` を補う |
| `--engine PATH` | — | USI エンジンのパス |
| `--hash MB` | — | エンジンの `USI_Hash` を上書き |
| `--ponder [true\|false]` | `true` | 先読み。値を省略したフラグ指定も `true` |
| `--floodgate [true\|false]` | `true` | 評価値・PV コメントを送信。値省略時も `true` |
| `--ws-origin ORIGIN` | — | WebSocket の Origin。通常のネイティブ接続では省略 |
| `--keep-alive SEC` | `30` | CSA 空行 / ロビーの `LOBBY_PONG` 間隔。0 で周期送信なし、正値は 30 以上 |
| `--margin-msec MS` | `1500` | 自分の思考予算から差し引く通信マージン |
| `--max-games N` | `0` | 最大対局数。0 は無制限 |
| `--log-level LEVEL` | `info` | `error` / `warn` / `info` / `debug` / `trace` |
| `--record-dir DIR` | `./records` | CSA / SFEN 保存先と既定の JSONL 保存先の親 |
| `--jsonl-out DIR` | — | JSONL 保存先だけを上書き（既定は `record.dir/jsonl/`） |
| `--options K=V,K=V,...` | — | USI オプション。同名キーは `--hash` よりこちらを優先 |
| `--engine-stderr-passthrough` | `false` | エンジン stderr を info ログにも出す |
| `--no-engine-stderr-passthrough` | — | 上記設定を無効化。両フラグ指定時は最後の指定を採用 |
| `-h`, `--help` | — | ヘルプを表示 |

表の既定値は TOML・環境変数・プリセットがない場合の値。
`--engine-stderr-passthrough=false` は受け付けない。

### 環境変数

`CSA_HOST`, `CSA_PORT`, `CSA_ID`, `CSA_PASSWORD` が使える。
シェルスクリプトでパスワードを設定ファイルに書きたくない場合に便利。

## 主な設定項目

### `[server]` — 接続先

```toml
[server]
host = "wdoor.c.u-tokyo.ac.jp"  # floodgate
port = 4081
id = "rshogi_v1"
password = "floodgate-300-10F,your_trip"  # floodgate は <game_name>,<trip> 形式（後述）
floodgate = true  # 評価値・PVコメントを送信
```

`host` の scheme で transport が決まる: ホスト名 / `tcp://ホスト名` と別設定の `port` は TCP、
`ws://...` / `wss://...` は WebSocket（Cloudflare Workers 版、`port` は無視）。
Workers 版への接続は
[Cloudflare Workers (WebSocket) で対局する](#cloudflare-workers-websocket-で対局する)
を参照。

### `[engine]` — USIエンジン

```toml
[engine]
path = "./target/release/rshogi-usi"
startup_timeout_sec = 30

[engine.options]
USI_Hash = 1024
Threads = 4
```

`[engine.options]` にはUSIエンジンが対応する任意のオプションを書ける。

### `[time]` — 時間管理

```toml
[time]
margin_msec = 1500  # 通信遅延を考慮した安全マージン
```

秒読み時計では `byoyomi` から、加算・切れ負け時計では手番側の `btime` / `wtime` から
差し引いてエンジンに渡す。相手の残時間は減らさない。bestmove がサーバーの期限前に
届くようにする通信マージンで、ネットワーク越しの対局では遅延に合わせて設定する。

> **エンジン側の遅延予約との二重計上に注意。** rshogi-usi など USI エンジンが
> `NetworkDelay` / `NetworkDelay2` を持つ場合、エンジンも内部で同種の予約を行うため
> `margin_msec` と**積み重なる**（秒読み 10s + `margin_msec=1500` + エンジン
> `NetworkDelay=120` なら実効思考は約 8.4s）。rshogi-usi と組むなら片方を小さくして
> 二重計上を避ける。一方、自前の遅延予約を持たないエンジンでは `margin_msec` が唯一の
> 安全弁になるので `0` にしない。

### `[game]` — 対局設定

```toml
[game]
max_games = 0       # 0 = 無制限に連続対局
ponder = true       # 相手手番中の先読み
restart_engine_every_game = false  # メモリリーク対策
```

接続失敗時は `[retry] initial_delay_sec = 10` から指数的に待ち時間を増やし、
`max_delay_sec = 900` で頭打ちにする。サーバーの `retry_after` が長ければそちらを待つ。
`[log]` の既定値は `level = "info"`、`dir = "./logs"`、`stdout = true`。
ログレベルは TOML または `--log-level` で設定する。

棋譜・JSONL の保存設定と再接続時の記録は[記録リファレンス](csa-client-records.md)を参照。

## 使い方の例

### floodgate で連続対局

実運用の全項目を含む設定例は **[docs/examples/csa-client-floodgate.toml.example](examples/csa-client-floodgate.toml.example)**
をコピーして使う(`~/floodgate/active.toml` のように OSS repo 外へ置き、名前・トリップ・
パスを自分の値へ差し替える。認証情報を含むため repo 内には置かない)。日常運用
(観戦・戦績集計・再ビルド) を 1 コマンド化する wrapper スクリプト例は
[docs/examples/floodgate/](examples/floodgate/README.md) にある。要点は接続部分:

```toml
[server]
host = "wdoor.c.u-tokyo.ac.jp"
port = 4081
id = "YourEngineName"
password = "floodgate-300-10F,your-secret-trip"  # <game_name>,<trip>(必須。理由は下記)
floodgate = true

[game]
max_games = 0
ponder = true
```

```bash
# バックグラウンド実行
nohup cargo run -p rshogi-csa-client --release -- config.toml > csa.log 2>&1 &
```

Ctrl-C (SIGINT) で待機・対局を中止して終了する。

> **floodgate の参加プールは `password` で選ぶ。** floodgate は参加したい対局条件
> （game_name = 持ち時間プリセット）を `password` 欄に **`<game_name>,<trip>`** 形式で
> 埋めて選択する（公式: `https://wdoor.c.u-tokyo.ac.jp/shogi/`）。`csa_client` は
> `%%GAME` を送らずサーバーの `Game_Summary` を待つため、**`password` に game_name が
> 無いと LOGIN は成功するがどのプールにも入れず、毎時 :00/:30 のペアリングを素通りして
> 対局が永久に始まらない。** `<game_name>` の現行名は公式ページで確認する（記載時点
> `floodgate-300-10F` = 持ち時間 300 秒 + 1 手 10 秒加算の Fischer）。`<trip>` は任意
> 文字列だがレーティング紐付けの識別子なので、同一エンジンでは `id` ともども固定する
> （変えると別人格扱いでレートが継続しない）。ログイン名は任意で事前登録は不要。持ち時間
> 自体はサーバーが `Game_Summary` で配信するため config には書かない。

### LAN 内の自前サーバーで対局（`rshogi-csa-server-tcp`）

本リポジトリ同梱の TCP 対局サーバー `rshogi-csa-server-tcp` を建てれば、CSA-over-TCP で
複数エンジンを対局させられる。起動・設定の詳細は
[`csa-server/tcp-server.md`](csa-server/tcp-server.md) を参照。

サーバー側（最小構成）。対局するハンドルとパスワードを `players.toml` に登録する:

```toml
# players.toml
[players.alice]
password = "alicepw"

[players.bob]
password = "bobpw"
```

```bash
cargo run -p rshogi-csa-server-tcp --release -- \
  --bind 0.0.0.0:4081 --kifu-dir ./kifu --players ./players.toml
# 既定は countdown 600 秒 + 10 秒読み（--clock-kind / --total-time-sec / --byoyomi-sec で変更）
```

クライアントは 2 台（または 2 プロセス）で接続する。LOGIN id は
`<handle>+<game_name>+<color>` 形式で、`password` は `players.toml` の値。
**同じ `<game_name>` + 逆の `<color>`** でマッチングする:

```toml
# 先手側
[server]
host = "192.168.1.100"   # サーバーの IP
port = 4081
id = "alice+casual-600-10+black"   # <handle>+<game_name>+<color>
password = "alicepw"
floodgate = false
```

```toml
# 後手側
[server]
host = "192.168.1.100"
port = 4081
id = "bob+casual-600-10+white"
password = "bobpw"
floodgate = false
```

`<game_name>`（例 `casual-600-10`）はマッチングのプール名。`--clock-presets-toml` を
渡さない限り任意の名前でよく、サーバーの global clock が全 game_name に適用される。

> floodgate（`wdoor.c.u-tokyo.ac.jp`）や Ruby [shogi-server](https://github.com/TadaoYamaoka/shogi-server)
> など外部 CSA サーバーにも同じ `csa_client` で接続できる（その場合は各サーバーの
> LOGIN 規約に従う。floodgate は本ドキュメント冒頭の例を参照）。

## Cloudflare Workers (WebSocket) で対局する

rshogi の対局場は Cloudflare Workers 版もあり、こちらは生 TCP ではなく
**WebSocket** で接続する。`csa_client` は `[server] host` の scheme で transport を
自動判別するので、`wss://` を指定すれば追加設定なしで WebSocket 接続になる
（`wss://` の TLS provider はバイナリ起動時に自動登録される）。

| transport | host の書き方 |
|---|---|
| TCP (floodgate / shogi-server) | `host = "wdoor.c.u-tokyo.ac.jp"` + `port` |
| WebSocket (Workers) | `host = "wss://<ホスト>/ws/<room_id>"`（`port` は無視） |

### LOGIN とマッチング

Workers 版の GameRoom は LOGIN ID を **`<handle>+<game_name>+<color>`** の 3 部構成で
要求する（`+` 区切り）。

| 部分 | 意味 |
|---|---|
| `<handle>` | 表示名（任意の文字列） |
| `<game_name>` | **時計プリセット名**。サーバ側 `CLOCK_PRESETS` に登録された名前を指定する |
| `<color>` | `black`（先手） / `white`（後手） |

`password` は LOGIN の**必須トークン**だが任意の文字列でよい（省略は不可。
`WORKERS_HANDLE_AUTH` で当該 handle を登録した場合のみ検証され、それ以外は認証に
使われない）。マッチングは 2 方式:

- **直接ルーム** `/ws/<room_id>`: 2 人が **同じ room_id・逆の color** で接続すると対局開始。
  `room_id` は ASCII 英数字・`-`・`_`、1〜128 文字。
- **ロビー** `/ws/lobby`（`--lobby`）: `LOGIN_LOBBY` 送信 → サーバが相手を見つけて
  `MATCHED` 通知 → 自動で割当ルームへ接続。**同じ `<game_name>` 同士**だけがマッチする。

> **先手・後手（`<color>`）は必ず互いに逆にすること。** サーバは color を自動割当・
> ランダム割当しないため、両者が `black` / `white` を明示し、かつ**反対の color** で
> なければマッチしない（直接ルーム・ロビーとも）。同じ `room_id` / `game_name` でも
> 2 人が同じ color を選ぶと対局は成立しない。「席おまかせ」での自動割当は未対応。

公開インスタンスで使える `<game_name>`（時計プリセット例）:

| game_name | 時計 |
|---|---|
| `floodgate-600-10` | 10 分 + 10 秒読み（Floodgate 互換） |
| `byoyomi-120-5` | 2 分 + 5 秒読み |
| `byoyomi-msec-10-100` | 10 秒 + 0.1 秒読み（高速 smoke 用） |

### 公開インスタンスに参加する

公開対局場の接続先:

```
wss://rshogi-csa-server.sh11235.com/ws/<room_id>
```

TOML 例（相手は同じ room へ `white` で接続する）:

```toml
[server]
host = "wss://rshogi-csa-server.sh11235.com/ws/myroom-001"
id = "alice+floodgate-600-10+black"   # <handle>+<game_name>+<color>
password = "anything"
floodgate = true

[engine]
path = "./target/release/rshogi-usi"

[engine.options]
Threads = 4
EvalFile = "/path/to/nn.bin"
```

```bash
cargo run -p rshogi-csa-client --release -- my_config.toml
```

バイナリ内蔵プリセット `--target production` でも同じサーバに繋げる（URL 指定不要）:

```bash
cargo run -p rshogi-csa-client --release -- \
  --target production \
  --room-id myroom-001 --handle alice --color black \
  --game-name floodgate-600-10 \
  --engine ./target/release/rshogi-usi \
  --options "Threads=4,EvalFile=/path/to/nn.bin"
```

ロビー自動マッチング（同じ `--game-name` の相手と順次対局）:

```bash
cargo run -p rshogi-csa-client --release -- \
  --target production --lobby \
  --handle alice --color black --game-name floodgate-600-10 \
  --max-games 0 \
  --engine ./target/release/rshogi-usi \
  --options "Threads=4,EvalFile=/path/to/nn.bin"
```

ロビーでは `--handle`・`--color`・`--game-name` が必要で、`--room-id` は不要。
`game_name` は ASCII 英数字・`-`・`_` の 1〜32 文字で、同梱環境では登録済み時計プリセットを指定する。
相手待ちに 60 秒の上限は設けない。`--keep-alive` の間隔で `LOBBY_PONG` を送って登録を維持し、
`LOBBY_PING` にも応答する。0 にすると周期送信は止まるため、サーバーの TTL 超過で登録が失効し得る。
`LOGIN_LOBBY:expired` や接続失敗後はバックオフして登録し直す。
私的対局のサーバープロトコルには `free` があるが、この CLI の `--color` は `black` / `white` のみ。
私的対局の招待・参加は[プロトコル参照](csa-server/protocol-reference.md#921-workers-の私的対局)を参照。

> `csa_client` はネイティブ経路（`Origin` ヘッダなし）で接続するため `ws_origin` は
> **指定しないこと**。公開インスタンスの `WS_ALLOWED_ORIGINS` はブラウザ製クライアント
> 向けの allowlist で、`ws_origin` を allowlist 外の値で指定すると `403` で弾かれる。

### 自前の Worker に接続する

`rshogi-csa-server-workers` を自分で deploy した場合は `--host`（または TOML）で
その URL を指定する。`--target` は本リポ単一アカウントの URL 固定なので使わない。

```toml
[server]
host = "wss://<your-worker>/ws/myroom-001"   # /ws/<room_id>
id = "alice+floodgate-600-10+black"           # <handle>+<game_name>+<color>
password = "anything"
floodgate = true
# Worker の WS_ALLOWED_ORIGINS が非空 かつ ブラウザ経路で繋ぐ場合のみ:
# ws_origin = "https://<allowlist に登録した origin>"
```

サーバ側の時計設定（`CLOCK_KIND` / `CLOCK_PRESETS`）と deploy 手順は
[`csa-server/clock_defaults.md`](csa-server/clock_defaults.md) /
[`csa-server/deployment.md`](csa-server/deployment.md) を参照。
`CLOCK_PRESETS = "[]"`（プリセット未登録）の Worker では `<game_name>` は任意で、
グローバル既定時計が全 game_name に適用される。観戦・ライブ対局 API は
[`csa-server/viewer_access_control.md`](csa-server/viewer_access_control.md) を参照。

## CSA 1.2.1 と大会前の確認

- `REJECT:<game_id>` は対局不成立として扱い、投了やログアウトを送らず同じ接続で次の `Game_Summary` を待つ。不成立の対局は対局数に含めない。
- `AGREE` 後の `START` 待ちは時間制限を設けず、設定した keep-alive と停止要求の確認を続ける。対局 ID が通知と一致しなければプロトコルエラーで終了する。停止済みなら AGREE を送らず終了し、Game_ID 省略時は START の確定 ID を棋譜と再接続用 summary に保存する。
- Ctrl-C は待機・対局を中止する要求であり、対局完了まで待つ要求ではない。AGREE 後の停止はサーバー側の対局成立を取り消さない。
- 進捗 API の `GameSummary` は対局候補ごとに発火する。REJECT 後は次候補で置き換え、`GameStarted` で成立を確定する。サーバーが START で ID を確定した場合は、`GameStarted` の直前に確定 ID を含む `GameSummary` を再通知する。
- 共通および先後別の `Time_Unit`（`sec` / `min` / `msec`、正の整数倍）を、開始手順・自手のエコー・相手手の時計更新に適用する。USI へはミリ秒で渡す。
- CSA 棋譜と進捗通知の既存 `time_sec` は秒へ換算し、秒未満を切り捨てる。時計の計算と開始手順・相手手の JSONL `elapsed_ms` はミリ秒精度を保持する。
- 入玉はサーバーの `Declaration:Jishogi 1.1` から `CSARule27` を設定する。ただし明示したエンジンオプションが優先されるため、大会用設定で別のルールを指定しないこと。

仕様: [CSA サーバ プロトコル ver.1.2.1](https://www.computer-shogi.org/protocol/tcp_ip_server_121.html)

## 棋譜・JSONL・出力

保存設定、ライブ観戦、再接続後の棋譜継続、出力形式は[記録リファレンス](csa-client-records.md)を参照。

### 終局理由の記録

[終局理由と CSA / JSONL の対応表](csa-client-records.md#終局理由の記録)へ移動した。
