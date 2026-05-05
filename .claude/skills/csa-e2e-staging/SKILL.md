---
description: Workers (`rshogi-csa-server-workers`) deploy 環境に対する `csa_client` 実機 E2E 手順。平手 1 局完走 / 連続対局 / 切断再接続 / 観戦 / Buoy 対局 / 異常終局 / 時計違いペアの 7 シナリオを通電させる。「staging で smoke 流して」「reconnect 検証して」「観戦テスト」「buoy 動かして」「異常終局再現」「時計 kind 切替確認」等のリクエストで起動する。
user-invocable: true
---

# CSA server E2E スキル (Workers staging / production)

`rshogi-csa-server-workers` を deploy した Worker (本リポ既定では
`rshogi-csa-server-workers-staging.<account>.workers.dev` /
`rshogi-csa-server-workers.<account>.workers.dev`) に対し、`csa_client` を 2
プロセス起動して各シナリオを実機で通電する。

## 0. 前提と環境変数

事前に以下が揃っていることを確認する。揃っていなければユーザに質問して埋めて
もらう:

- **Cloudflare account 名** (`<account>`): 本リポ標準では `sh11235`。OSS 利用者
  が独自 deploy している場合は別の値を入れる。CSA Worker URL の subdomain
  (`rshogi-csa-server-workers-staging.<account>.workers.dev`) と一致する。
- **Worker deploy 状態**: `vp exec wrangler deploy --config wrangler.staging.toml`
  または `gh workflow run deploy-workers.yml -f target=staging` 済。`/health` で
  生存確認:
  ```bash
  curl -sf https://rshogi-csa-server-workers-staging.<account>.workers.dev/health
  ```
- **CLOCK_PRESETS 既定**: 同梱の `wrangler.{staging,production}.toml` に登録された
  3 preset (`byoyomi-msec-10-100` / `byoyomi-120-5` / `floodgate-600-10`) が
  使える。追加 preset が必要なシナリオは事前に `CLOCK_PRESETS` を編集して
  再 deploy する。詳細は `docs/csa-server/clock_defaults.md`。
- **csa_client release ビルド**: `target/release/csa_client` を確認。未生成なら
  `cargo build --release -p rshogi-csa-client` を実行する (~20 秒)。
- **USI エンジン + options**: NNUE モデル付きの本番想定構成。下表は本リポで
  運用している YaneuraOu sfnnwop1536 の例 (engine path / options は user に
  確認):

  | キー | 例 | 役割 |
  |---|---|---|
  | engine path | `/path/to/YaneuraOu-sfnnwop1536-...-tournament` | release 版 USI engine binary |
  | `EvalDir` | `/path/to/eval_v100_300` | NNUE 評価関数ディレクトリ |
  | `LS_PROGRESS_COEFF` | `/path/to/progress_hao_full_cuda.e1.bin` | progress8kpabs 係数ファイル |
  | `FV_SCALE` | `28` | 評価値スケール |
  | `LS_BUCKET_MODE` | `progress8kpabs` | LayerStack bucket 選択 |
  | `BookFile` | `no_book` | 定跡無効化 (E2E では決定論性を上げる) |
  | `NetworkDelay` / `NetworkDelay2` | `0` | ネット遅延補正 OFF |
  | `MinimumThinkingTime` | `1000` (本番) / `100` (短時間 preset 向け) | 1 手最低思考時間 ms |
  | `PvInterval` | `0` | PV 出力間隔 |
  | `Threads` | `1` | 単スレ |
  | `USI_Hash` | `512` | TT サイズ MB |

  HalfKP 系 (suisho5 等) を使う場合は `EvalDir` の代わりに `EvalFile` を渡し、
  `LS_*` / `FV_SCALE` 系は engine が無視するか必須でないかを user に確認する。
- **`--game-name` と `--room-id` の関係** (strict mode 必須事項):
  - `--target` 経路で `--game-name <preset>` を渡すと LOGIN id は
    `<handle>+<preset>+<color>` になる (URL の `<room_id>` とは独立)
  - `--game-name` 省略時は `<room_id>` を `<game_name>` として fallback
    (`CLOCK_PRESETS = "[]"` の Worker や lobby 慣習向け)
  - `<room_id>` を任意文字列 (`e2e-<timestamp>` 等) にする場合は **必ず
    `--game-name <preset>` を併指定** すること。さもないと strict mode で
    `LOGIN_LOBBY:incorrect unknown_game_name` 拒否
- **Floodgate 系機能 (再接続 / R2 / viewer API)** が有効: 両 wrangler.toml の
  `RECONNECT_GRACE_SECONDS` / `ALLOW_FLOODGATE_FEATURES` / `ALLOW_VIEWER_API` を
  確認。staging / production 既定では C シナリオが追加 deploy なしで通電する。

## 1. 共通セットアップ

別ターミナルで Worker tail を流すと R2 export ログ
(`[GameRoom] kifu exported to R2 key=...`) や error がリアルタイムで見える:

```bash
vp exec wrangler tail \
  --config crates/rshogi-csa-server-workers/wrangler.staging.toml \
  --format pretty
```

`csa_client` の起動方法は **2 通り** 選べる:

### 1-A. CLI プリセット (`--target`) で TOML なし起動

最短経路。本リポ同梱 Worker 限定。`--max-games 1` で 1 局終了で client が自動
quit する。`<room_id>` は黒/白で完全一致、`<preset>` も完全一致 (黒/白 LOGIN
のマッチング条件)。

```bash
ROOM=e2e-$(date +%Y%m%d%H%M%S)
PRESET=floodgate-600-10
ACC=<account>  # 本リポでは sh11235
ENGINE=/path/to/your/usi-engine
# YaneuraOu sfnnwop1536 用 options 例 (HalfKP 系なら EvalFile に置き換え):
OPTS="EvalDir=/path/to/eval_v100_300,FV_SCALE=28,LS_BUCKET_MODE=progress8kpabs,LS_PROGRESS_COEFF=/path/to/progress.bin,BookFile=no_book,NetworkDelay=0,NetworkDelay2=0,MinimumThinkingTime=1000,PvInterval=0,Threads=1,USI_Hash=512"

# 黒
target/release/csa_client \
  --target staging \
  --room-id "$ROOM" \
  --handle alice \
  --color black \
  --game-name "$PRESET" \
  --engine "$ENGINE" \
  --options "$OPTS" \
  --max-games 1 &

# 白 (room_id / game-name を黒と同じ値で揃える)
target/release/csa_client \
  --target staging \
  --room-id "$ROOM" \
  --handle bob \
  --color white \
  --game-name "$PRESET" \
  --engine "$ENGINE" \
  --options "$OPTS" \
  --max-games 1 &
wait
```

### 1-B. TOML 設定で起動

スキーマと最小例: `crates/rshogi-csa-client/examples/csa_client.toml.example`。

```bash
cp crates/rshogi-csa-client/examples/csa_client.toml.example /tmp/black.toml
cp crates/rshogi-csa-client/examples/csa_client.toml.example /tmp/white.toml
target/release/csa_client /tmp/black.toml &
target/release/csa_client /tmp/white.toml &
wait
```

各 toml で **必ず** 書き換える項目 (placeholder のままでは動かない):

| field | 役割 | 書き換え例 |
|---|---|---|
| `server.host` | Worker URL。末尾 `/ws/<room_id>` の `<room_id>` を黒/白で完全一致させる | `wss://rshogi-csa-server-workers-staging.<account>.workers.dev/ws/e2e-20260505...` |
| `server.id` | LOGIN handle。`<handle>+<game_name>+<color>` の `<game_name>` は **必ず CLOCK_PRESETS 登録 preset 名**(strict mode、未登録名は `LOGIN_LOBBY:incorrect unknown_game_name` で reject される) | 黒: `alice+floodgate-600-10+black` / 白: `bob+floodgate-600-10+white` |
| `engine.path` | ローカル USI engine 絶対パス | `/abs/path/to/your/usi-engine` |
| `engine.options` 内 `EvalFile` 等 | 実機 engine が要求するモデルパス | engine 仕様による |

## 2. シナリオ A: 平手 1 局完走

最小 smoke。CLI 経路 (1-A) なら `--max-games 1` で、TOML 経路 (1-B) なら
`[game] max_games = 1` で 1 局終了で client が自動 quit する。

preset 選び:

| preset | 完走時間目安 | 終局理由の傾向 |
|---|---|---|
| `byoyomi-msec-10-100` | 数秒〜30 秒程度 | 1 手最低思考が秒読み (100ms) を超えるエンジンでは `#TIME_UP` 着地が普通 (smoke として終局到達自体を見る用途向け) |
| `byoyomi-120-5` | 数分 | 平和な終局 (`%TORYO`) を狙うならこちら |
| `floodgate-600-10` | 10〜25 分 | 本番想定の長尺、棋力統計用途向け |

期待観測:

- 両 client log (info レベル) に下記の順で出る (`game_id` は `[CSA] 対局情報受信:
  <game_id> ...` の行から取り出せる):
  - `[CSA] ログイン成功: <id>`
  - `[CSA] 対局情報受信: <game_id> ...`
  - `[CSA] 対局開始: START:<game_id>`
  - 終局時 `[CSA] 対局終了: #WIN` または `[CSA] サーバー終局割り込み: Lose`
- ローカル `records/` (CLI なら `--record-dir`、TOML なら `[record] dir`) 配下に
  `<datetime>_<sente>_vs_<gote>.csa` + `.sfen` が保存される
- R2 bucket (`rshogi-csa-kifu-staging` または `-prod`) に同 game_id の object が
  追加される。viewer API で取得確認:

```bash
curl -sf "https://rshogi-csa-server-workers-staging.<account>.workers.dev/api/v1/games/<game_id>" \
  | python3 -c "import json,sys; d=json.load(sys.stdin)['meta']; print(f'end_reason={d[\"end_reason\"]} result={d[\"result_kind\"]} moves={d[\"moves_count\"]}')"
```

`end_reason` が `RESIGN` / `TIME_UP` / `ILLEGAL_MOVE` / `ABNORMAL` のいずれか、
`source: "floodgate"`、CSA file の `BEGIN Time` block が想定 preset と一致する
ことを確認。

## 3. シナリオ B: 連続 N 対局

DO instance = 1 対局の設計のため、終局後の同 room_id 再 LOGIN は reject される。
連続対局は `host` URL の room_id 末尾と handle に `{game_seq}` placeholder を
入れることで対応する (csa_client が 0,1,2,...,(max_games-1) を埋める)。

```toml
host = "wss://rshogi-csa-server-workers-staging.<account>.workers.dev/ws/myroom-{game_seq}"
id = "alice-{game_seq}+byoyomi-msec-10-100+black"
[game]
max_games = 5
```

期待: client ログに `対局 #1 〜 #5 結果` が並び、R2 に 5 件追加される。

## 4. シナリオ C: 切断 → 再接続

> **前提**: `RECONNECT_GRACE_SECONDS > 0` + `ALLOW_FLOODGATE_FEATURES = "true"`。
> 本リポ既定の staging / production はどちらも 30 秒 grace + opt-in 有効化済。

preset は `byoyomi-120-5` (中時間) を選び、grace 30 秒の中で reconnect 操作の
余裕を確保する。

手順:

1. シナリオ A 構成で起動して対局を進める
2. 黒 client ログから `Reconnect_Token:<token>` と `Game_ID:<game_id>` を抜き取る
   (debug ログレベル `[CSA] < ` プレフィクスで出る)
3. 黒 client を `Ctrl+C` または `kill -KILL <pid>` で停止 (server 側 grace
   timer 開始)
4. `wrangler tail` で `[GameRoom] entered grace window: role=Black grace_secs=30`
   を確認
5. **次のいずれかの経路で grace + reconnect 動作を観測する** (用途で使い分け):

   **(a) Server 側 grace + force_abnormal の実機検証** (推奨、最も確実):
   - 黒 client を `kill -KILL <pid>` で process ごと停止
   - `wrangler tail` で `[GameRoom] entered grace window` → 30 秒経過後に
     `force_abnormal` ログを確認
   - 白 client が `#ABNORMAL` + `#WIN` を受信して終局
   - R2 export には `end_reason: "ABNORMAL"`、viewer API
     (`GET /api/v1/games/<game_id>`) で同値を確認
   - **これだけで server 側 grace 機構 (Issue #607-#609 の核心) は完全に
     検証できる**。reconnect 成立まで見ない、grace 期限切れ経路の検証として
     十分。

   **(b) 真の resume (token を使った再接続成功) を検証** (advanced):
   - csa_client は WS Close を検知すると保持済 token を使って自動再接続する
     (実装: `crates/rshogi-csa-client/src/main.rs::attempt_reconnect`、
     `LOGIN <id> <pw> reconnect:<game_id>+<token>` を送出 → 受理時 server が
     `BEGIN Game_Summary` + `BEGIN Reconnect_State` を送り返す。protocol 詳細は
     `docs/csa-server/protocol-reference.md` §9.1 参照)
   - そのため process は生かしたまま **WS Close だけ発生させる** 必要がある。
     ただし手元で動く確実な手段は限定的:
     - **`sudo tc qdisc add dev <iface> root netem loss 100%` で全 egress を
       一時遮断**: root 必須 + machine 全体の通信を巻き込む。CI / sandbox 専用。
       直後に `sudo tc qdisc del dev <iface> root` で復元
     - **socat proxy 経由で localhost に向ける構成**: csa_client が wss → proxy
       (TLS 終端) → wss → Worker と中継させる。ただし Cloudflare Workers は
       `Host:` ヘッダを SNI と一致させる検証を行うため、単純な
       `socat TCP-LISTEN:8443 OPENSSL:<host>:443` では Host が `localhost:8443`
       のままになり Worker ルーティングが失敗しがち。動かす場合は前段 nginx
       (`proxy_set_header Host <worker-host>`) で Host を書き換える必要があり、
       OSS 利用者向けの汎用手順としては未提供。実機試したいなら csa_client 側
       に `--simulate-disconnect-after-msec` 等のテスト用フラグを追加して
       process 内部から WS を close する path を作るのが最終的に安定。
   - 手動 wscat から再接続 LOGIN 行を組み立てる場合は
     `LOGIN <handle>+<preset>+<color> <pw> reconnect:<game_id>+<token>`。
     csa_client の TOML id 経由ではこの形式の LOGIN は未対応 (csa_client は
     auto-reconnect 経路のみで `login_reconnect` を呼ぶ)。

   通常の動作確認では **(a) で十分**。OSS 利用者が真の resume 機構を E2E
   検証したい場合のみ (b) を上記 caveat 付きで使う。
6. 終局後に R2 棋譜を確認すると **1 つの game_id** に黒の disconnect 前後の
   指し手が連続している

### grace 検証だけでよい場合 (process kill で足りる経路)

`#608/#609` のような server 側 grace + alarm 動作確認だけなら、process kill
してそのまま 30 秒 grace 期限切れを待ち `force_abnormal` (= `#ABNORMAL` / 相手
`#WIN`) を観測すれば検証目的は達成できる。R2 export には
`end_reason: "ABNORMAL"` が記録される。

## 5. シナリオ D: 観戦 (`%%MONITOR2ON`)

`csa_client` は観戦モード未実装のため `wscat` 等の汎用 WS client で擬似する。

```bash
wscat -c "wss://rshogi-csa-server-workers-staging.<account>.workers.dev/ws/<room_id>"
> LOGIN spectator+<preset_name>+spectator anything
< LOGIN:spectator+<preset_name>+spectator OK
> %%MONITOR2ON <game_id>
< [対局者の指し手が broadcast される]
```

シナリオ A と並行起動し、対局者の指し手が spectator 側にも届くことを確認する。
`<preset_name>` は対局と同じ preset 名を使う (strict mode 下では preset 登録
されていない game_name は LOGIN_LOBBY:incorrect で reject される)。

## 6. シナリオ E: Buoy 対局 (`%%SETBUOY` / `%%DELETEBUOY`)

ADMIN 権限で運用権限コマンドを送り、中盤局面からの対局を成立させる。
`csa_client` は管理コマンドを直接送る経路を持たないので ADMIN 部分は `wscat`
で代替し、対局者は通常の `csa_client` を使う。

```bash
# `<ADMIN_HANDLE>` は staging/production の wrangler secret に設定された値。
wscat -c "wss://rshogi-csa-server-workers-staging.<account>.workers.dev/ws/<room_id>"
> LOGIN <ADMIN_HANDLE>+<preset_name>+black anything
< LOGIN:... OK
> %%SETBUOY <preset_name> +7776FU -3334FU 1
```

`count = 1` なので 1 回だけ buoy 対局が成立する。続いて通常 client 2 本で対局
すると、Game_Summary の `Position` ブロックに `+7776FU` `-3334FU` の 2 手が
適用された中盤局面が入る。`%%GETBUOYCOUNT <preset_name>` を再度送ると count が
0 になり、再対局できないことを確認する。

> **strict mode の注意**: `<preset_name>` は CLOCK_PRESETS に登録された値で
> あること。SETBUOY の `<game_name>` と LOGIN handle の `<game_name>` を一致
> させる必要がある。

## 7. シナリオ F: 異常切断系

シナリオ A 実行中に以下を起こすと各終局理由がトリガーされる:

| 操作 | 期待される終局 | R2 棋譜末尾 |
|---|---|---|
| 黒 client を `Ctrl+C` で kill (grace 無効構成、または grace 期限切れ) | `#ABNORMAL` + 相手 `#WIN` | `%CHUDAN` |
| `byoyomi (100ms) + α` だけ engine 思考を遅らせる | `#TIME_UP` + `#LOSE/#WIN` | `#TIME_UP` 中間行 + 結果行 |
| 不正な指し手を送る (`wscat` 経由で `+7775FU` 等の無効手) | `#ILLEGAL_MOVE` + `#LOSE/#WIN` | `#ILLEGAL_MOVE` |

`#TIME_UP` を 100ms byoyomi で再現するには engine 思考を 100ms より遅くするのが
最短。NNUE モデル未 load または `Hash = 1024` 等の memory pressure で 1 手目に
間に合わない局面を作れる。`#ILLEGAL_MOVE` は `wscat` 経由が確実
(csa_client は engine の合法手しか送らないため)。

## 8. シナリオ G: 時計 kind 切替確認

CLOCK_PRESETS のおかげで以前のように `wrangler.toml` の `CLOCK_KIND` を mutate
する必要は無い。各 kind を preset として登録すれば LOGIN 時の `<game_name>`
切替だけで kind を選べる:

```toml
CLOCK_PRESETS = '''[
  {"game_name":"byoyomi-msec-10-100","kind":"countdown_msec","total_time_ms":10000,"byoyomi_ms":100},
  {"game_name":"byoyomi-120-5","kind":"countdown","total_time_sec":120,"byoyomi_sec":5},
  {"game_name":"floodgate-600-10","kind":"countdown","total_time_sec":600,"byoyomi_sec":10},
  {"game_name":"fischer-300-10F","kind":"fischer","total_time_sec":300,"increment_sec":10},
  {"game_name":"stopwatch-10-1M","kind":"stopwatch","total_time_min":10,"byoyomi_min":1}
]'''
```

各 preset で 1 局ずつ走らせ、Game_Summary `BEGIN Time` セクションの `Time_Unit` /
`Total_Time` / `Byoyomi`/`Increment` が想定値であることを確認する。

> 旧 staging-e2e.md にあった `sed -i 's/CLOCK_KIND = .*/.../'` で wrangler.toml
> を mutate する手順は **使わない**。CLOCK_PRESETS で全 kind を共存できる
> 設計に変わったため。

## 9. 後始末

1. R2 検証用 object を削除したい場合 (残しても害はない):
   ```bash
   vp exec wrangler r2 object delete \
     rshogi-csa-kifu-staging/<key> \
     --config crates/rshogi-csa-server-workers/wrangler.staging.toml
   ```
2. ローカルの `/tmp/<scenario>-*.toml` / `./records/` 配下を必要に応じて破棄

## トラブルシューティング

| 症状 | 原因候補 | 対処 |
|---|---|---|
| `403 Forbidden Origin` で WS Upgrade 失敗 | csa_client の `ws_origin` が `WS_ALLOWED_ORIGINS` allowlist に含まれていない | `ws_origin` を toml から削除 (ネイティブ経路 / Origin 欠落) するか、wrangler.toml の allowlist に Origin を追加して再 deploy |
| `LOGIN:incorrect` | `<handle>+<game_name>+<color>` の format 違反、または `<game_name>` が CLOCK_PRESETS 未登録 (strict mode) | id format を再確認、`<game_name>` を登録 preset 名に変更 |
| `LOGIN_LOBBY:incorrect unknown_game_name` | strict mode 下で未登録 game_name | CLOCK_PRESETS に追加して再 deploy、または既存 preset 名を使う |
| 双方接続するも対局が始まらない | `room_id` が黒/白で不一致 | URL `/ws/<room_id>` が両 toml で完全一致しているか確認 |
| 対局終局後も R2 に書き込まれない | 終局イベントが落ちた | `vp exec wrangler tail` で error 確認 |
| `#TIME_UP` で対局終了 (通常実行で意図せず) | engine 応答が server byoyomi に間に合わない | csa_client の `[time] margin_msec` を上げる (engine 渡し byoyomi がその分減り余裕生まれる) |

## 関連実装 / doc

- 設定 schema: `crates/rshogi-csa-client/examples/csa_client.toml.example`
- README: `crates/rshogi-csa-client/examples/README.md` (CLI / lobby / JSONL モード解説)
- clock 設計: `docs/csa-server/clock_defaults.md`
- protocol 詳細: `docs/csa-server/protocol-reference.md`
- lobby DO 詳細: `docs/csa-server/lobby_e2e_runbook.md`
- viewer API: `docs/csa-server/viewer_access_control.md`
- 自動再接続実装: `crates/rshogi-csa-client/src/main.rs::attempt_reconnect`
- server grace 経路: `crates/rshogi-csa-server-workers/src/game_room.rs::enter_grace_window`
