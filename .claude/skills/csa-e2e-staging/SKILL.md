---
name: csa-e2e-staging
description: "Workers (`rshogi-csa-server-workers`) deploy 環境に対する `csa_client` 実機 E2E 手順。平手 1 局完走 / 連続対局 / 切断再接続 / 観戦 / Buoy 対局 / 異常終局 / 時計違いペアの 7 シナリオを通電させる。「staging で smoke 流して」「reconnect 検証して」「観戦テスト」「buoy 動かして」「異常終局再現」「時計 kind 切替確認」等のリクエストで起動する。"
user-invocable: true
---

# CSA server E2E スキル (Workers staging / production)

`rshogi-csa-server-workers` を deploy した Worker に対し、`csa_client` を 2 プロセス
起動して各シナリオを実機で通電する。Worker は `workers_dev = false` で workers.dev
直 URL を公開しないため、到達経路は `wrangler.{staging,production}.toml` の `routes`
が宣言する Custom Domain のみ。fork は自分の route の host に読み替える。

## 0a. シナリオ選定 (どの change にどの scenario を流すか)

7 シナリオすべてを毎回流す必要はない。**変更した経路に対応する subset** を
選んで走らせるのが原則。A は smoke 通電基本として常に走らせる。

| 変更内容 | 必要 scenario | メモ |
|---|---|---|
| Worker DO logic 全般 (game_room.rs / lobby.rs / config.rs) | **A** | smoke だけで充分なケース多数 |
| reconnect grace / token (`enter_grace_window` / `Reconnect_Token` 周辺) | **A + C** | grace 経路 regression check |
| AGREE / start_match / agree_timeout | **A + F (agree_timeout sub-case)** | LOGIN→Game_Summary→AGREE 待ちの alarm 検証 |
| spectator broadcast (`%%MONITOR2ON`) | **A + D** | A と並走で broadcast 確認 |
| Buoy / private match (`%%SETBUOY`) | **A + E** | admin command 経路 |
| clock kind / preset 設計 (`CLOCK_PRESETS`) | **A + G** | preset 切替で `Time_Unit` / `Total_Time` を確認 |
| csa_client 専用変更 (engine.rs / send 経路 / record 経路) | **A** | client 単独 smoke |
| 連続対局 (`--max-games` ループ、`{game_seq}` 経路) | **A + B** | client 内部だが Workers 1 room = 1 対局制約とも整合 |
| 上記を複数横断する大規模変更 | **A + 関連全部** | 該当 scenario を選ぶ |

**「変更経路と無関係な scenario」は走らせない**。trivia な確認は CI / unit
test (`pnpm test:smoke` の miniflare smoke 等) 側で済んでいる前提で、本 skill は
実機しか取れない通電を最小化して取る。

## 0. 前提と環境変数

事前に以下が揃っていることを確認する。揃っていなければユーザに質問して埋めて
もらう:

- **到達 host**: wrangler toml の `routes` が正典。以降の手順は `CSA_HOST` を使う:
  ```bash
  WRANGLER=crates/rshogi-csa-server-workers/wrangler.staging.toml   # production は wrangler.production.toml
  CSA_HOST=$(sed -n 's/^routes = \[{ pattern = "\([^"]*\)".*/\1/p' "$WRANGLER")
  test -n "$CSA_HOST" || echo "routes の pattern を読めない: $WRANGLER"
  ```
- **Worker deploy 状態**: `vp exec wrangler deploy --config wrangler.staging.toml`
  または `gh workflow run deploy-workers.yml -f target=staging` 済。`/health` で
  生存確認:
  ```bash
  curl -sf "https://$CSA_HOST/health"
  ```
- **CLOCK_PRESETS**: 登録済み preset は wrangler toml から読む (skill に一覧を
  写さない):
  ```bash
  grep -o '"game_name":"[^"]*"' "$WRANGLER"
  ```
  1 件以上登録されていると strict mode で、未登録の `game_name` は
  `LOGIN_LOBBY:incorrect unknown_game_name` で拒否される。追加 preset が必要な
  シナリオは事前に `CLOCK_PRESETS` を編集して再 deploy する。仕様は
  `docs/csa-server/clock_defaults.md`。
- **csa_client release ビルド**: `target/release/csa_client` を確認。未生成なら
  `cargo build --release -p rshogi-csa-client` を実行する。
- **USI エンジン + options**: 既定は rshogi (`engines/rshogi-usi-<edition>` または
  `target/release/rshogi-usi`)。NNUE モデル・FV_SCALE はモデルごとに異なるので
  user に確認する。

  | キー | 例 | 役割 |
  |---|---|---|
  | engine path | `engines/rshogi-usi-<edition>` | release / production profile の USI engine |
  | `EvalFile` | `$SHOGI_DATA/nnue/<model>.bin` | NNUE 評価関数 |
  | `FV_SCALE` | モデルの記録値 | 評価値スケール (明示する) |
  | `LS_BUCKET_MODE` | `progresskpabs` | LayerStack bucket 選択 (LayerStacks モデルでは明示必須) |
  | `LS_PROGRESS_BUCKETS` | 学習時の値 (例 `8`) | progresskpabs の routing bucket 数 (明示必須) |
  | `LS_PROGRESS_COEFF` | `$SHOGI_DATA/progress/<coeff>.bin` | progresskpabs 係数ファイル |
  | `BookFile` | `no_book` | 定跡無効化 (rshogi の既定も `no_book`) |
  | `NetworkDelay` / `NetworkDelay2` | `0` | ネット遅延補正 OFF |
  | `MinimumThinkingTime` | `1000` (本番) / `100` (短時間 preset 向け) | 1 手最低思考時間 ms |
  | `Threads` | `1` | 単スレ |
  | `USI_Hash` | `512` | TT サイズ MB |

  LayerStacks モデルで `LS_BUCKET_MODE` を省くと既定が無いので `isready` で panic
  する。progresskpabs なら `LS_PROGRESS_BUCKETS` と `LS_PROGRESS_COEFF` も必須。
  HalfKP 等の非 LayerStacks モデルでは `LS_*` は不要 (指定不可)。
  YaneuraOu 等の外部 engine を使う場合は、その engine のオプション名
  (`EvalDir` 等) に読み替える。
- **`--game-name` と `--room-id` の関係** (strict mode 必須事項):
  - `--target` 経路で `--game-name <preset>` を渡すと LOGIN id は
    `<handle>+<preset>+<color>` になる (URL の `<room_id>` とは独立)
  - `--game-name` 省略時は `<room_id>` を `<game_name>` として fallback
    (`CLOCK_PRESETS = "[]"` の Worker や lobby 慣習向け)
  - `<room_id>` を任意文字列 (`e2e-<timestamp>` 等) にする場合は **必ず
    `--game-name <preset>` を併指定** すること。さもないと strict mode で
    `LOGIN_LOBBY:incorrect unknown_game_name` 拒否
- **Floodgate 系機能 (再接続 / R2 / viewer API)** が有効: wrangler toml の
  `RECONNECT_GRACE_SECONDS` / `ALLOW_FLOODGATE_FEATURES` / `ALLOW_VIEWER_API` を
  確認 (`grep -nE 'RECONNECT_GRACE_SECONDS|ALLOW_FLOODGATE_FEATURES|ALLOW_VIEWER_API|AGREE_TIMEOUT_SECONDS' "$WRANGLER"`)。

## 1. 共通セットアップ

別ターミナルで Worker tail を流すと R2 export ログ
(`[GameRoom] kifu exported to R2 key=...`) や error がリアルタイムで見える:

```bash
vp exec wrangler tail --config "$WRANGLER" --format pretty
```

`csa_client` の起動方法は **2 通り** 選べる。どちらも **起動直後に PID を記録**し、
生存確認・停止は PID (`ps -p "$PID"` / `kill "$PID"`) で行う。

### 1-A. CLI プリセット (`--target`) で TOML なし起動

最短経路。本リポ同梱 Worker 限定。`--max-games 1` で 1 局終了で client が自動
quit する。`<room_id>` は黒/白で完全一致、`<preset>` も完全一致 (黒/白 LOGIN
のマッチング条件)。

```bash
RUN=runs/csa-e2e/$(date +%Y%m%d-%H%M%S)
mkdir -p "$RUN"
ROOM=e2e-$(date +%Y%m%d%H%M%S)
PRESET=<登録済み preset 名>
ENGINE=engines/rshogi-usi-<edition>
OPTS="EvalFile=$SHOGI_DATA/nnue/<model>.bin,FV_SCALE=<value>,LS_BUCKET_MODE=progresskpabs,LS_PROGRESS_BUCKETS=8,LS_PROGRESS_COEFF=$SHOGI_DATA/progress/<coeff>.bin,BookFile=no_book,NetworkDelay=0,NetworkDelay2=0,MinimumThinkingTime=1000,Threads=1,USI_Hash=512"

target/release/csa_client --target staging --room-id "$ROOM" --handle alice \
  --color black --game-name "$PRESET" --engine "$ENGINE" --options "$OPTS" \
  --max-games 1 > "$RUN/black.log" 2>&1 &
echo $! > "$RUN/black.pid"

# 白 (room_id / game-name を黒と同じ値で揃える)
target/release/csa_client --target staging --room-id "$ROOM" --handle bob \
  --color white --game-name "$PRESET" --engine "$ENGINE" --options "$OPTS" \
  --max-games 1 > "$RUN/white.log" 2>&1 &
echo $! > "$RUN/white.pid"
wait
```

### 1-B. TOML 設定で起動

スキーマと最小例: `crates/rshogi-csa-client/examples/csa_client.toml.example`
(各 field の意味はこのファイルのコメントが正典)。

```bash
cp crates/rshogi-csa-client/examples/csa_client.toml.example "$RUN/black.toml"
cp crates/rshogi-csa-client/examples/csa_client.toml.example "$RUN/white.toml"
# 編集後:
target/release/csa_client "$RUN/black.toml" > "$RUN/black.log" 2>&1 &
echo $! > "$RUN/black.pid"
target/release/csa_client "$RUN/white.toml" > "$RUN/white.log" 2>&1 &
echo $! > "$RUN/white.pid"
wait
```

必ず書き換える項目: `server.host` (`wss://$CSA_HOST/ws/<room_id>`、黒/白で完全一致)、
`server.id` (`<handle>+<登録済み preset>+<color>`)、`engine.path`、`engine.options`
内のモデルパス。placeholder のままでは動かない。

## 2. シナリオ A: 平手 1 局完走

最小 smoke。CLI 経路 (1-A) なら `--max-games 1` で、TOML 経路 (1-B) なら
`[game] max_games = 1` で 1 局終了で client が自動 quit する。

preset 選びは時計の性質で決める (名前は上の `grep` で確認):

| 時計 | 完走時間目安 | 終局理由の傾向 |
|---|---|---|
| ミリ秒秒読み (`countdown_msec`、10 秒 + 100ms 等) | 数秒〜30 秒程度 | 1 手最低思考が秒読みを超えるエンジンでは `#TIME_UP` 着地が普通 (終局到達自体を見る smoke 向け) |
| 短い秒読み (120 秒 + 5 秒等) | 数分 | 平和な終局 (`%TORYO`) を狙うならこちら |
| floodgate 相当 (600 秒 + 10 秒) | 10〜25 分 | 本番想定の長尺、棋力統計用途向け |

期待観測:

- 両 client log (info レベル) に下記の順で出る (`game_id` は `[CSA] 対局情報受信:
  <game_id> ...` の行から取り出せる):
  - `[CSA] ログイン成功: <id>`
  - `[CSA] 対局情報受信: <game_id> ...`
  - `[CSA] 対局開始: START:<game_id>`
  - 終局時 `[CSA] 対局終了: #WIN` または `[CSA] サーバー終局割り込み: Lose`
- ローカル `records/` (CLI なら `--record-dir`、TOML なら `[record] dir`) 配下に
  `<datetime>_<sente>_vs_<gote>.csa` + `.sfen` が保存される
- R2 bucket に同 game_id の object が追加される。viewer API で取得確認:

```bash
curl -sf "https://$CSA_HOST/api/v1/games/<game_id>" \
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
host = "wss://<CSA_HOST>/ws/myroom-{game_seq}"
id = "alice-{game_seq}+<preset>+black"
[game]
max_games = 5
```

期待: client ログに `対局 #1 〜 #5 結果` が並び、R2 に 5 件追加される。

## 4. シナリオ C: 切断 → 再接続

> **前提**: `RECONNECT_GRACE_SECONDS > 0` + `ALLOW_FLOODGATE_FEATURES = "true"`。

preset は中時間 (120 秒 + 5 秒等) を選び、grace の中で reconnect 操作の余裕を確保する。

手順:

1. シナリオ A 構成で起動して対局を進める
2. 黒 client ログから `Reconnect_Token:<token>` と `Game_ID:<game_id>` を抜き取る
   (debug ログレベル `[CSA] < ` プレフィクスで出る)
3. 記録した PID で黒 client を停止する: `kill -KILL "$(cat "$RUN/black.pid")"`
4. `wrangler tail` で `[GameRoom] entered grace window: role=Black grace_secs=<N>`
   を確認
5. **次のいずれかの経路で grace + reconnect 動作を観測する** (用途で使い分け):

   **(a) Server 側 grace + force_abnormal の実機検証** (推奨、最も確実):
   - `wrangler tail` で `[GameRoom] entered grace window` → grace 経過後に
     `force_abnormal` ログを確認
   - 白 client が `#ABNORMAL` + `#WIN` を受信して終局
   - R2 export には `end_reason: "ABNORMAL"`、viewer API
     (`GET /api/v1/games/<game_id>`) で同値を確認
   - これだけで server 側 grace 機構は検証できる。reconnect 成立までは見ない
     grace 期限切れ経路の検証として十分。

   **(b) 真の resume (token を使った再接続成功) を検証** (advanced):
   - csa_client は WS Close を検知すると保持済 token を使って自動再接続する
     (実装: `crates/rshogi-csa-client/src/main.rs::attempt_reconnect`、
     `LOGIN <id> <pw> reconnect:<game_id>+<token>` を送出 → 受理時 server が
     `BEGIN Game_Summary` + `BEGIN Reconnect_State` を送り返す。protocol 詳細は
     `docs/csa-server/protocol-reference.md` §9.1 参照)
   - そのため process は生かしたまま **WS Close だけ発生させる** 必要がある。
     手元で確実なのは `sudo tc qdisc add dev <iface> root netem loss 100%` で
     egress を一時遮断する方法のみ (root 必須、machine 全体の通信を巻き込むので
     CI / sandbox 専用。直後に `sudo tc qdisc del dev <iface> root` で復元)。
   - 手動 wscat から再接続 LOGIN 行を組み立てる場合は
     `LOGIN <handle>+<preset>+<color> <pw> reconnect:<game_id>+<token>`。
     csa_client の TOML id 経由ではこの形式の LOGIN は未対応 (csa_client は
     auto-reconnect 経路のみで `login_reconnect` を呼ぶ)。

   通常の動作確認では **(a) で十分**。真の resume 機構を E2E 検証したい場合のみ
   (b) を上記 caveat 付きで使う。
6. (b) の場合、終局後に R2 棋譜を確認すると **1 つの game_id** に黒の disconnect
   前後の指し手が連続している

## 5. シナリオ D: 観戦 (`%%MONITOR2ON`)

`csa_client` は観戦モード未実装のため `wscat` 等の汎用 WS client で擬似する。

```bash
wscat -c "wss://$CSA_HOST/ws/<room_id>"
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
wscat -c "wss://$CSA_HOST/ws/<room_id>"
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

| 操作 | 期待される終局 | R2 棋譜末尾 / 備考 |
|---|---|---|
| 黒 client を記録 PID で kill (grace 無効構成、または grace 期限切れ) | `#ABNORMAL` + 相手 `#WIN` | `%CHUDAN` |
| byoyomi + α だけ engine 思考を遅らせる | `#TIME_UP` + `#LOSE/#WIN` | `#TIME_UP` 中間行 + 結果行 |
| 不正な指し手を送る (`wscat` 経由で `+7775FU` 等の無効手) | `#ILLEGAL_MOVE` + `#LOSE/#WIN` | `#ILLEGAL_MOVE` |
| 両 LOGIN 後 AGREE を送らず TTL 待ち | 両 player に `##[ERROR] agree_timeout` + WS close (code 1011 "match aborted") | **R2 export なし**、live-games-index にも残らない (`play_started_at_ms` None で skip) |

`#TIME_UP` をミリ秒秒読み preset で再現するには engine 思考を秒読みより遅くするのが
最短。NNUE モデル未 load や大きな `USI_Hash` の memory pressure で 1 手目に
間に合わない局面を作れる。`#ILLEGAL_MOVE` は `wscat` 経由が確実
(csa_client は engine の合法手しか送らないため)。

### `agree_timeout` sub-case の最短再現手順

`csa_client` は LOGIN→Game_Summary→自動 AGREE まで一気通貫で走るため、AGREE を
**送らない** 動作には別ツールが必要。workers crate 同梱の probe を使う:

```bash
cd crates/rshogi-csa-server-workers
pnpm install
CSA_TEST_PASSWORD=<pw> node scripts/agree_timeout_probe.mjs \
  "wss://$CSA_HOST/ws" "e2e-agree-$(date +%H%M%S)" <ミリ秒秒読み preset> 90
```

両 session が `##[ERROR] agree_timeout` を受信して close されれば exit 0、
それ以外 (LOGIN 拒否・接続失敗・待ち時間切れ) は exit 1。待ち時間は
`AGREE_TIMEOUT_SECONDS` より十分長く取る。

期待観測:

- 両 session に `LOGIN:... OK` → `BEGIN Game_Summary` ... `Reconnect_Token:...` →
  `END Game_Summary` まで普通に届く
- AGREE を送らず `AGREE_TIMEOUT_SECONDS` 経過後、両 session に
  `##[ERROR] agree_timeout` 受信
- 続いて WS close (code 1011, reason "match aborted")
- viewer API: `live_games` に残らない、`/api/v1/games/<game_id>` は **404**
  (R2 export 自体が起きない)

`AGREE_TIMEOUT_SECONDS` の値は wrangler toml の `[vars]` を参照 (staging と
production で異なる)。サーバー側ロジックの回帰は
`tests/miniflare_smoke/agree_timeout.test.ts` が local で押さえている。

## 8. シナリオ G: 時計 kind 切替確認

`wrangler.toml` の `CLOCK_KIND` を mutate する必要は無い。各 kind を
`CLOCK_PRESETS` に preset として登録すれば LOGIN 時の `<game_name>` 切替だけで
kind を選べる。登録例の形式:

```toml
CLOCK_PRESETS = '''[
  {"game_name":"byoyomi-msec-10-100","kind":"countdown_msec","total_time_ms":10000,"byoyomi_ms":100},
  {"game_name":"byoyomi-120-5","kind":"countdown","total_time_sec":120,"byoyomi_sec":5},
  {"game_name":"fischer-300-10F","kind":"fischer","total_time_sec":300,"increment_sec":10},
  {"game_name":"stopwatch-10-1M","kind":"stopwatch","total_time_min":10,"byoyomi_min":1}
]'''
```

各 preset で 1 局ずつ走らせ、Game_Summary `BEGIN Time` セクションの `Time_Unit` /
`Total_Time` / `Byoyomi`/`Increment` が想定値であることを確認する。

## 9. 後始末

1. R2 検証用 object を削除したい場合 (残しても害はない):
   ```bash
   vp exec wrangler r2 object delete <bucket>/<key> --config "$WRANGLER"
   ```
   bucket 名は wrangler toml の `r2_buckets` を参照する。
2. `$RUN` 配下の toml / log / pid と `./records/` を必要に応じて破棄

## 運用上の落とし穴 (実機で踏みやすい点)

### `csa_client` ログは INFO レベルに **対局中の指し手は出ない**

INFO ログに見えるのは「ログイン成功」「対局情報受信」「対局開始」「対局終了」
のような lifecycle event のみ。対局中に新しいログが出ないのは正常。生存確認は
記録した PID の `ps -p "$(cat "$RUN/black.pid")"` か `wrangler tail` で行う。
プロセス名のパターン一致 (`pgrep -f` 等) は監視ループ自身のコマンドラインに
一致して終わらなくなるので使わない。

### Scenario C で kill から `force_abnormal` までの所要は **grace 秒数ではなく
~100 秒**

`kill -KILL` は CSA-level の disconnect message を送らないため、server 側は
TCP/WS keep-alive のタイムアウトで切断を検知する。Cloudflare WS の idle
timeout は概ね 70〜90 秒なので、`kill -KILL` から `enter_grace_window` までで
70〜90 秒、その後 `RECONNECT_GRACE_SECONDS` を待って `force_abnormal`
発火する。合計 100〜120 秒程度を見込む。grace 値だけを基準に待つと早合点する。

### 並列 background process は `()` subshell で detach すると wait が効かない

`( cmd > log 2>&1 & )` は外側 subshell の中で background させるため、外側 subshell
は即 exit し、`wait` も `$!` も拾えない。**素直に `&` だけで background 化**し、
直後に `echo $! > <pidfile>` で PID を残す。

## トラブルシューティング

| 症状 | 原因候補 | 対処 |
|---|---|---|
| `403 Forbidden Origin` で WS Upgrade 失敗 | csa_client の `ws_origin` が `WS_ALLOWED_ORIGINS` allowlist に含まれていない | `ws_origin` を toml から削除 (ネイティブ経路 / Origin 欠落) するか、wrangler.toml の allowlist に Origin を追加して再 deploy |
| `LOGIN:incorrect` | `<handle>+<game_name>+<color>` の format 違反、または `<game_name>` が CLOCK_PRESETS 未登録 (strict mode) | id format を再確認、`<game_name>` を登録 preset 名に変更 |
| `LOGIN_LOBBY:incorrect unknown_game_name` | strict mode 下で未登録 game_name | CLOCK_PRESETS に追加して再 deploy、または既存 preset 名を使う |
| 双方接続するも対局が始まらない | `room_id` が黒/白で不一致 | URL `/ws/<room_id>` が両 toml で完全一致しているか確認 |
| 対局終局後も R2 に書き込まれない | 終局イベントが落ちた | `vp exec wrangler tail` で error 確認 |
| `#TIME_UP` で対局終了 (通常実行で意図せず) | engine 応答が server byoyomi に間に合わない | csa_client の `[time] margin_msec` を上げる (engine 渡し byoyomi がその分減り余裕生まれる) |
| `rshogi-usi` が `LS_BUCKET_MODE must be explicitly set` / `requires LS_PROGRESS_COEFF` で panic | LayerStacks routing 未指定 (既定なし) | options に `LS_BUCKET_MODE=progresskpabs,LS_PROGRESS_BUCKETS=<学習時 bucket 数>,LS_PROGRESS_COEFF=<係数ファイル>` を追加。§0 engine 表下のメモ参照 |
| AGREE timeout を `csa_client` で再現できない | csa_client は LOGIN→Game_Summary→自動 AGREE まで一気通貫 | §7 `agree_timeout` sub-case の probe script を使う |

## 関連実装 / doc

- 設定 schema: `crates/rshogi-csa-client/examples/csa_client.toml.example`
- README: `crates/rshogi-csa-client/examples/README.md` (CLI / lobby / JSONL モード解説)
- clock 設計: `docs/csa-server/clock_defaults.md`
- protocol 詳細: `docs/csa-server/protocol-reference.md`
- lobby DO 詳細: `docs/csa-server/lobby_e2e_runbook.md`
- viewer API: `docs/csa-server/viewer_access_control.md`
- 自動再接続実装: `crates/rshogi-csa-client/src/main.rs::attempt_reconnect`
- server grace 経路: `crates/rshogi-csa-server-workers/src/game_room.rs::enter_grace_window`
- AGREE timeout: `crates/rshogi-csa-server-workers/src/game_room.rs::handle_agree_timeout_alarm`
- AGREE timeout probe: `crates/rshogi-csa-server-workers/scripts/agree_timeout_probe.mjs`
- AGREE timeout 設定 env: `wrangler.{staging,production,toml.example}.toml` の `AGREE_TIMEOUT_SECONDS`
