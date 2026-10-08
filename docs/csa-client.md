# CSA対局クライアント (`csa_client`)

USIエンジンをCSAプロトコル対局サーバー（floodgate等）にCLIから接続するツール。
将棋所などのGUIを介さず、バックグラウンドで連続対局を実行できる。

## CSA 1.2.1 と大会前の確認

- `REJECT:<game_id>` は対局不成立として扱い、投了やログアウトを送らず同じ接続で次の `Game_Summary` を待つ。不成立の対局は対局数に含めない。
- `AGREE` 後の `START` 待ちは時間制限を設けず、設定した keep-alive と停止要求の確認を続ける。対局 ID が通知と一致しなければプロトコルエラーで終了する。停止済みなら AGREE を送らず終了し、Game_ID 省略時は START の確定 ID を棋譜と再接続用 summary に保存する。
- Ctrl-C は待機・対局を中止する要求であり、対局完了まで待つ要求ではない。AGREE 後の停止はサーバー側の対局成立を取り消さない。
- 進捗 API の `GameSummary` は対局候補ごとに発火する。REJECT 後は次候補で置き換え、`GameStarted` で成立を確定する。
- 共通および先後別の `Time_Unit`（`sec` / `min` / `msec`、正の整数倍）を、開始手順・自手のエコー・相手手の時計更新に適用する。USI へはミリ秒で渡す。
- CSA 棋譜と進捗通知の既存 `time_sec` は秒へ換算し、秒未満を切り捨てる。時計の計算と開始手順・相手手の JSONL `elapsed_ms` はミリ秒精度を保持する。
- 入玉はサーバーの `Declaration:Jishogi 1.1` から `CSARule27` を設定する。ただし明示したエンジンオプションが優先されるため、大会用設定で別のルールを指定しないこと。

電竜戦向けの回帰テストは、冒頭4手の時間調整後に先手180秒・後手600秒となる設定と毎手2秒加算、両色の最初の探索、`%KACHI` の送信、512手目のエコー後の `#MAX_MOVES` → `#DRAW` の終局通知、残時間1秒と2秒加算、エンジンを再利用した次の対局を模擬サーバーで確認する。実大会サーバーへの接続試験は別途行うこと。これは CSA 1.2.1 の全オプションへの完全対応を保証するものではない。

仕様: [CSA サーバ プロトコル ver.1.2.1](https://www.computer-shogi.org/protocol/tcp_ip_server_121.html)

## クイックスタート

```bash
# 1. 設定ファイルを用意（example をコピーして編集）
cp crates/rshogi-csa-client/examples/csa_client.toml.example my_config.toml
# → server.host, server.id, server.password, engine.path を書き換える

# 2. 実行
cargo run -p rshogi-csa-client --release -- my_config.toml
```

## 設定

TOML設定ファイルで管理する。`crates/rshogi-csa-client/examples/csa_client.toml.example` に全設定項目のコメント付き例がある（TCP / WebSocket 両経路対応）。

### 設定の優先順位

CLIオプション > 環境変数 > TOML設定ファイル > デフォルト値

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

主なオプション:

| オプション | 説明 |
|-----------|------|
| `--host` | CSAサーバーホスト名 |
| `--port` | ポート番号 |
| `--id` | ログインID |
| `--password` | パスワード |
| `--engine` | USIエンジンのパス |
| `--hash` | USI_Hash (MB) |
| `--ponder` | ponder 有効/無効 |
| `--floodgate` | floodgateモード（評価値コメント送信） |
| `--keep-alive` | keep-alive間隔（秒） |
| `--margin-msec` | 秒読みマージン（ms） |
| `--max-games` | 最大対局数（0=無制限） |
| `--log-level` | ログレベル (error/warn/info/debug/trace) |
| `--record-dir` | 棋譜保存ディレクトリ |
| `--options` | USIオプション (K=V,K=V,...) |

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

`host` の scheme で transport が決まる: `host:port` / `tcp://...` は TCP、
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

秒読みからこの値を差し引いてエンジンに渡す（`btime`/`wtime` は満額）。bestmove が
サーバ側 deadline 前に届くようにするための csa_client 層のバッファで、ネットワーク
越しの対局では大きめに設定する。

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

### `[record]` — 棋譜保存

```toml
[record]
enabled = true
dir = "./records"
filename_template = "{datetime}_{sente}_vs_{gote}"
save_csa = true   # CSA形式
save_sfen = true  # SFEN局面列（学習データ生成用）
save_jsonl = true # analyze_selfplay / kifu_player 互換 JSONL（dir/jsonl/ に 1 局 1 ファイル）
live_jsonl = false # 対局中に JSONL を手単位で live 追記（下記）
```

テンプレート変数: `{datetime}`, `{game_id}`, `{sente}`, `{gote}`

`live_jsonl = true` にすると、対局中に JSONL ファイル（`save_jsonl` と同じパス）へ
確定した手を 1 手ずつ追記する。`kifu_player --tournament-dir <dir>/jsonl --live` で
開いておくと、**自エンジンの進行中対局を評価値付き・手単位でリアルタイム観戦**できる
（wdoor を経由しないので低遅延）。追記中のファイルには result 行が無く、読者は
進行中対局（勝敗不明）として扱う。終局時は従来どおりの完全な内容（result 行込み）へ
置き換わる。追記の失敗は警告のみで対局は続行する。`save_jsonl = false` なら効果なし。

#### 再接続時の棋譜継続

対局中の切断から `Reconnect_Token` で復帰した場合、`csa_client` は切断前にメモリへ
蓄積した指し手を対局開始局面から replay し、再送された `Game_Summary` の現在局面
（盤面・持ち駒・手番）および `Reconnect_State` の `Current_Turn` と照合する。
一致すれば開始時刻と既存の指し手・探索情報を
引き継ぎ、再接続を挟んでも `.csa` / `.sfen` / `.jsonl` を 1 局 1 ファイルの通し記録
として保存する。ファイル名の `{datetime}` も最初の対局開始時刻のまま変わらない。

局面または game ID・対局者・自手番が一致しない場合は `[REC] resume 局面不一致` の
警告を出し、resume 局面以降だけを断片として保存する。この場合は全形式のファイル名に
`_reconnect_fragment` が付き、CSA には `RECONNECT_FRAGMENT` コメントと不一致理由も
記録される。

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

Ctrl+C (SIGINT) で現在の対局完了後にgracefulに終了する。

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

## 出力

### ログ

```
[2026-03-28T12:00:00.123Z INFO] CSA対局クライアント起動
[2026-03-28T12:00:00.456Z INFO] [CSA] ログイン成功: rshogi_v1
[2026-03-28T12:00:30.789Z INFO] [CSA] 対局開始: START:game123
[2026-03-28T12:10:15.012Z INFO] 対局 #1 結果: Win | 通算: 1勝 0敗 0分
```

`--log-level debug` で CSA/USI 通信の全行が表示される。

### 棋譜ファイル

`records/` ディレクトリに対局ごとに保存:

- `20260328_120030_rshogi_v1_vs_opponent.csa` — CSA形式（評価値コメント付き）
- `20260328_120030_rshogi_v1_vs_opponent.sfen` — SFEN局面列（タブ区切り: SFEN, 指し手, 評価値）

内蔵 CSA サーバーについても、ミリ秒・分時計の着手通知と `Time_Unit` の一致を
実 `GameRoom` と TCP 接続による統合テストで検証する。対局者の残時間はサーバーの
課金結果に一致し、保存する CSA V2 棋譜の消費時間は秒単位を維持する。
