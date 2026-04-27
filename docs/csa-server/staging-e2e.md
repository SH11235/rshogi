# Workers staging × csa_client 実機対局 E2E 手順書

CSA-over-WebSocket で Cloudflare Workers の staging サーバーに `csa_client` を
複数接続し、運用想定の各シナリオを実機で通電させる手順をまとめる。短時間対局
向けに staging は `CLOCK_KIND = "countdown_msec"`、`BYOYOMI_MS = "100"` で
deploy されており、平手 1 局を数秒〜十数秒で完走できる。

deployment 全体像は [`deployment.md`](deployment.md) を参照。

## 0. 共通の前提

- staging Worker (`rshogi-csa-server-workers-staging.<account>.workers.dev`)
  が deploy 済みであること（`CLOCK_KIND = "countdown_msec"` / `BYOYOMI_MS = "100"`）。
- ローカルに USI エンジン（`rshogi-usi` 等）の release バイナリがあり、`/path/to/rshogi-usi`
  で起動できること。設定は最低限 `MaterialLevel = 1` で起動可能（NNUE モデル不要）。
- `vp exec wrangler` で staging Worker / R2 bucket を操作できる権限があること。
- `csa_client` の WS 経路（`tungstenite` 依存）が main に取り込まれていること。
- 各シナリオ用の sample TOML は
  [`crates/rshogi-csa-client/examples/csa_client_staging/scenarios/`](../../crates/rshogi-csa-client/examples/csa_client_staging/scenarios/)
  配下に scenario 別ディレクトリで配置されている。

## 1. 運用シナリオ一覧

| ID | シナリオ | 目的 | 検証ポイント | サーバ側設定 | サンプル TOML |
| --- | --- | --- | --- | --- | --- |
| A | 平手 1 局完走 (short byoyomi) | 基本通電の最終確認 | LOGIN→Game_Summary→指し手交換→`%TORYO`→`#WIN/#LOSE`→R2 棋譜 | 既定 (countdown_msec / 100ms) | `scenarios/A_basic_one_game/` |
| B | 連続 N 対局 (`max_games=5`) | DO state 健全性、game_id 重複なし | 5 回 LOGIN→対局→終局を繰り返す、R2 に 5 件 | 既定 | `scenarios/B_consecutive_games/` |
| C | 切断→再接続 | reconnect protocol の通電 | 黒側を kill→ grace 内に同一トークンで復帰→対局継続 | `RECONNECT_GRACE_SECONDS > 0` + `ALLOW_FLOODGATE_FEATURES = "true"` 必要 | `scenarios/C_reconnect/` |
| D | 観戦 (`%%MONITOR2ON`) | 観戦経路 / spectator broadcast | spectator client が指し手を受信、対局者の挙動には影響なし | 既定 | wscat 用手順 (§6) |
| E | Buoy 対局 (`%%SETBUOY`) | ADMIN 権限 / 中盤局面開始 / count 減算 | admin が SETBUOY → 通常 client × 2 が中盤局面で対局開始、count 1 → 0 | `ADMIN_HANDLE` secret 必要 | `scenarios/E_buoy/` |
| F | 異常切断系 (`%CHUDAN`/`#TIME_UP`/`#ILLEGAL_MOVE`) | 各終局理由の R2 保存 | 棋譜末尾コマンドが正しい、R2 にも残る | 既定 | A シナリオの操作で再現 |
| G | 時計違い (countdown / fischer / stopwatch) | 各 `CLOCK_KIND` の `Time_Unit` / 経過計算 | wrangler を切り替えて再 deploy、Game_Summary の表記差分を確認 | `wrangler.staging.toml::CLOCK_KIND` 変更要 | `scenarios/G_clock_variants/` |

## 2. 共通セットアップ

### 2-1. ローカル csa_client 用 TOML 準備

```bash
# 例: シナリオ A (平手 1 局完走)
cp crates/rshogi-csa-client/examples/csa_client_staging/scenarios/A_basic_one_game/black.toml.example \
   /tmp/A-black.toml
cp crates/rshogi-csa-client/examples/csa_client_staging/scenarios/A_basic_one_game/white.toml.example \
   /tmp/A-white.toml
```

各 `.toml` の `engine.path` をローカル `rshogi-usi` の絶対パスに、
`host` URL の `<account>` 部分を Cloudflare アカウント名に、
`<room_id>` 部分を実行ごとに新規生成する任意文字列（例: `e2e-$(date +%Y%m%d%H%M%S)`）に
置換する。`id` の suffix（黒/白で異なる）も同じ `<room_id>` を入れて揃える:
`<handle>+<room_id>+<color>`。

### 2-2. Worker tail を別ターミナルで流す

```bash
vp exec wrangler tail \
  --config crates/rshogi-csa-server-workers/wrangler.staging.toml \
  --format pretty
```

R2 export ログ (`[GameRoom] kifu exported to R2 key=...`) や error が
リアルタイムで見える。

## 3. シナリオ A: 平手 1 局完走

短 byoyomi で 1 局を完走させ、終局後 R2 に棋譜が保存されることを確認する
基本シナリオ。`max_games = 1` で client は 1 局終了で自動 quit する。

### 3-1. 実行

ターミナル A（黒番）:
```bash
cargo run -p rshogi-csa-client --release -- /tmp/A-black.toml
```
ターミナル B（白番）:
```bash
cargo run -p rshogi-csa-client --release -- /tmp/A-white.toml
```

### 3-2. 期待ログ

両 client に以下が順に流れる:
```
[CSA/WS] 接続中: wss://...workers.dev/ws/<room_id>
[CSA/WS] 接続成功: status=101 Switching Protocols
[CSA] ログイン成功: <handle>+<room_id>+<color>
[CSA] 対局待機中...
[CSA] 対局情報受信: <game_id> ...
[CSA] 対局開始: START:<game_id>
[CSA] 対局終了: #WIN     (or #LOSE)
[REC] 棋譜保存: ./records/staging-e2e/<datetime>_<sente>_vs_<gote>.csa
対局 #1 結果: Win | 通算: 1勝 0敗 0分
最大対局数 (1) に達しました
```

### 3-3. R2 棋譜確認

```bash
vp exec wrangler r2 object list rshogi-csa-kifu-staging \
  --config crates/rshogi-csa-server-workers/wrangler.staging.toml | head
# 直近の object キー（例: 2026/04/27/<game_id>.csa）を取得
vp exec wrangler r2 object get \
  rshogi-csa-kifu-staging/2026/04/27/<game_id>.csa \
  --config crates/rshogi-csa-server-workers/wrangler.staging.toml \
  --file /tmp/<game_id>.csa
cat /tmp/<game_id>.csa
```

CSA V2 形式（`V2.2`、`N+`、`$GAME_ID:`、`BEGIN Position` 〜 `END Position`、
指し手 `+7776FU,T<sec>` 等、終局コマンド `%TORYO` / `+SUMI` 等）が含まれていれば成功。

> ※ Floodgate 履歴バケット (`rshogi-csa-floodgate-history-staging`) への
> 書き込みは staging では既定 (`ALLOW_FLOODGATE_FEATURES = "false"`) で
> 無効化されているため、本シナリオの必須確認項目には含めない。Floodgate
> 機能 opt-in 環境で動作確認する場合は別途
> `vp exec wrangler r2 object list rshogi-csa-floodgate-history-staging` で
> 確認する。

## 4. シナリオ B: 連続 N 対局

`max_games = 5` で同 client が 5 局繰り返す。**Workers 版 (`rshogi-csa-server-workers`)**
は 1 DO instance = 1 対局 という設計で、終局後の同 room_id への再 LOGIN は
`LOGIN:incorrect` で reject される (`game_room.rs::handle_login::load_finished`)。
連続対局では host / id 内の `{game_seq}` placeholder を csa_client が
0 始まりの局番号で自動置換するので、`<room_id>-{game_seq}` 形式に書いて
毎局新規 DO を立てる運用にする。

> 補足: TCP 版 (`rshogi-csa-server-tcp`) は本家 Floodgate 互換で 1 server に
> 多対局が成立するため、`{game_seq}` placeholder は不要 (むしろ host が
> DNS 名のため `tcp://host-0:4081` のような展開は不正な host になる)。
> TCP 経路で連続対局するときは host を固定し、game_name を毎局変えれば良い。

```bash
cp crates/rshogi-csa-client/examples/csa_client_staging/scenarios/B_consecutive_games/black.toml.example /tmp/B-black.toml
cp crates/rshogi-csa-client/examples/csa_client_staging/scenarios/B_consecutive_games/white.toml.example /tmp/B-white.toml
# 黒・白で同じ <room_id> base を入れて起動。`{game_seq}` は csa_client が
# 0,1,2,...,(max_games-1) を埋める。
cargo run -p rshogi-csa-client --release -- /tmp/B-black.toml &
cargo run -p rshogi-csa-client --release -- /tmp/B-white.toml &
wait
```

期待: 各 client ログに `対局 #1 〜 #5 結果` が並び、`通算: ...勝 ...敗 ...分`
が出る。R2 list で `<room_id>-<n>-<timestamp>.csa` 形式の object が **5 件**
追加されている。各 object の `game_id` は `<room_id>-<n>-<timestamp>` で
`<n>` が 0..4 の連番、`<timestamp>` が DO 内で発番される時刻 suffix。

## 5. シナリオ C: 切断 → 再接続

> **前提**: staging で `ALLOW_FLOODGATE_FEATURES = "true"` と
> `RECONNECT_GRACE_SECONDS = "30"` 等を設定して再 deploy する必要がある。
> 現在の staging は `false` / `0` で disabled。Floodgate features を有効化する
> 別 PR を merge してから本シナリオを実機検証する。

設定後の手順:

1. シナリオ A を起動して対局を進める。
2. Game_Summary 末尾の拡張行 `Reconnect_Token:<token>` を黒/白それぞれの
   client ログから抜き取る（debug ログで `[CSA] < ` プレフィクス付きで表示する）。
3. 黒 client を `Ctrl+C` で kill する（grace 内）。
4. 黒側の TOML の `id` を
   `<handle>+<room_id>+<color> reconnect:<game_id>+<token>` 形式に書き換え、
   再起動する。
5. 黒 client が `LOGIN:<name> OK` を受けて対局が継続する（指し手の続きから）。
6. 終局後に R2 棋譜を確認すると、**1 つの game_id** に黒の disconnect 前後の
   指し手が連続している。

> 注意: `RECONNECT_GRACE_SECONDS` で指定した秒数 (例: 30 秒) 以内に
> step 3 → 4 を完走する必要がある。手元で `id` 文字列を組み立てる作業を含む
> ため時間的余裕は少ない。事前に `<game_id>` と `<token>` を埋めた reconnect
> 用 id 文字列を別ターミナルに貼り付けておき、step 3 で kill した直後に
> step 4 の `id` だけ書き換えて再起動できるよう準備しておくのが安全。

## 6. シナリオ D: 観戦 (`%%MONITOR2ON`)

`csa_client` は観戦モードを未実装のため、`wscat` 等の汎用 WS client で擬似する。

```bash
# wscat (npm i -g wscat 等で導入) で staging に Spectator として接続。
# `--header "Origin: ..."` は staging の WS_ALLOWED_ORIGINS にマッチさせる場合のみ必要。
# Origin ヘッダを送らないネイティブ経路 (`--header` を省略) でも素通しで観戦できる。
wscat -c "wss://rshogi-csa-server-workers-staging.<account>.workers.dev/ws/<room_id>" \
  --header "Origin: https://csa-client-local"
> LOGIN spectator+<room_id>+spectator anything
< LOGIN:spectator+<room_id>+spectator OK
> %%MONITOR2ON <game_id>
< [対局者の指し手が broadcast される]
```

シナリオ A と並行起動し、対局者の指し手が spectator 側にも届くことを確認する。

## 7. シナリオ E: Buoy 対局 (`%%SETBUOY` / `%%DELETEBUOY`)

ADMIN_HANDLE 権限で運用権限コマンドを送り、中盤局面からの対局を成立させる。

`csa_client` は管理コマンドを直接送る経路を持たないので、ADMIN 権限部分は
`wscat` で代替し、対局者は通常の `csa_client` を使う。

### 7-1. ADMIN client から `%%SETBUOY` を送る

```bash
# `<ADMIN_HANDLE>` は staging の secret に設定された値。
# `vp exec wrangler secret list --config wrangler.staging.toml` で名前のみ確認。
# `--header "Origin: ..."` は allowlist 経路で確認したい場合のみ必要 (省略でも素通し)。
wscat -c "wss://rshogi-csa-server-workers-staging.<account>.workers.dev/ws/<room_id>" \
  --header "Origin: https://csa-client-local"
> LOGIN <ADMIN_HANDLE>+<game_name>+black anything
< LOGIN:... OK
> %%SETBUOY <game_name> +7776FU -3334FU 1
```

`count = 1` なので 1 回だけ buoy 対局が成立する。

### 7-2. 通常 client × 2 で対局

```bash
cp crates/rshogi-csa-client/examples/csa_client_staging/scenarios/E_buoy/black.toml.example /tmp/E-black.toml
cp crates/rshogi-csa-client/examples/csa_client_staging/scenarios/E_buoy/white.toml.example /tmp/E-white.toml
# id の `<game_name>` を SETBUOY と同じ値にする。
cargo run -p rshogi-csa-client --release -- /tmp/E-black.toml &
cargo run -p rshogi-csa-client --release -- /tmp/E-white.toml &
```

期待: Game_Summary の `Position` ブロックに `+7776FU` `-3334FU` の 2 手が
適用された中盤局面が入っている。`%%GETBUOYCOUNT <game_name>` を再度送ると
count が 0 になっており、再対局はできない（`%%SETBUOY count 1` のため）。

## 8. シナリオ F: 異常切断系

シナリオ A を実行中に以下を起こすと各終局理由がトリガーされる。

| 操作 | 期待される終局理由 | R2 棋譜末尾 |
| --- | --- | --- |
| 黒 client を `Ctrl+C` で kill | サーバ側で WS close → `#CHUDAN` | `%CHUDAN` |
| `byoyomi (100ms) + α` だけ engine 思考を遅らせる | サーバ側で時間切れ判定 → `#TIME_UP` + `#LOSE/#WIN` | `#TIME_UP` の中間行 + 結果行 |
| 不正な指し手を送る (例: `+7775FU` のような無効手) | `#ILLEGAL_MOVE` + `#LOSE/#WIN` | `#ILLEGAL_MOVE` |

`#TIME_UP` を再現するには engine 思考を 100ms より遅くするのが最短。
`MaterialLevel = 1` で十分軽量だが、`Hash = 1024` 等で memory pressure を
かけると間に合わない手が出るケースもある。

`#ILLEGAL_MOVE` は `wscat` 経由で手動で `+7775FU` のような無効手を送る方が
確実 (csa_client は engine の生成する合法手しか送らないため)。

## 9. シナリオ G: 時計違いペア

`wrangler.staging.toml` の `CLOCK_KIND` を切り替えて再 deploy し、シナリオ A
を各 kind で 1 局ずつ走らせる。

```bash
# countdown (Floodgate 互換、整数秒)
sed -i 's/CLOCK_KIND = .*/CLOCK_KIND = "countdown"/' \
  crates/rshogi-csa-server-workers/wrangler.staging.toml
gh workflow run "Deploy Workers" -f target=staging
# deploy 完了後、シナリオ A を実行。Game_Summary に Time_Unit:1sec が出る。

# fischer (秒、増分加算)
sed -i 's/CLOCK_KIND = .*/CLOCK_KIND = "fischer"/' \
  crates/rshogi-csa-server-workers/wrangler.staging.toml
gh workflow run "Deploy Workers" -f target=staging
# Game_Summary に Time_Unit:1sec / Increment:10 が出る。

# stopwatch (分、分単位切り捨て)
sed -i 's/CLOCK_KIND = .*/CLOCK_KIND = "stopwatch"/' \
  crates/rshogi-csa-server-workers/wrangler.staging.toml
gh workflow run "Deploy Workers" -f target=staging
# Game_Summary に Time_Unit:1min が出る。

# 検証完了後、countdown_msec に戻す（staging 既定）。
sed -i 's/CLOCK_KIND = .*/CLOCK_KIND = "countdown_msec"/' \
  crates/rshogi-csa-server-workers/wrangler.staging.toml
gh workflow run "Deploy Workers" -f target=staging
```

各 kind で R2 棋譜の `BEGIN Time` セクションが想定通りの `Time_Unit` /
`Total_Time` / `Byoyomi`/`Increment` を含むことを確認する。

> ⚠️ `sed -i` は **追跡対象** の `wrangler.staging.toml` を直接書き換える。
> 検証を中断して工程を中座した場合、ファイルが意図せぬ kind のまま残り
> 後続のコミットに混入する事故が起こり得る。各 sed の直後と最後の戻し sed
> の後で必ず以下を実行し、`countdown_msec` に戻っていることを目視確認する:
>
> ```bash
> git diff crates/rshogi-csa-server-workers/wrangler.staging.toml
> ```
>
> 戻ってない場合は `git checkout -- crates/rshogi-csa-server-workers/wrangler.staging.toml`
> で復元してから再 deploy する。

## 10. 後始末

1. R2 bucket の検証用 object（`csa_e2e_*` / `e2e-*` 等のキーで判別）を削除したい場合:
   ```bash
   vp exec wrangler r2 object delete \
     rshogi-csa-kifu-staging/<key> \
     --config crates/rshogi-csa-server-workers/wrangler.staging.toml
   ```
   残しておいても害はない（staging は volume 制限内で運用継続可能）。
2. ローカルに残った `/tmp/<scenario>-*.toml` / `./records/staging-e2e/`
   配下の棋譜ファイルを破棄する。
3. シナリオ G で `CLOCK_KIND` を変更した場合は **必ず** `countdown_msec` に
   戻して再 deploy する（staging 既定値）。

## トラブルシューティング

| 症状 | 原因候補 | 対処 |
| --- | --- | --- |
| `CSAサーバー接続失敗: WebSocket Upgrade 失敗` (`403 Forbidden Origin`) | csa_client が `ws_origin` を設定し、その値が `WS_ALLOWED_ORIGINS` 許可リストに含まれていない | `ws_origin` を toml から削除する（ネイティブ経路として素通し）か、`wrangler.staging.toml` の allowlist に Origin を追加して再 deploy |
| `ログイン失敗: LOGIN:incorrect` | `<handle>+<game_name>+<color>` 形式違反、または同 game_name で role 重複 | `id` の format を再確認、`<room_id>` を新規生成 |
| 双方接続するも対局が始まらない | `room_id` が黒/白で不一致 | URL の `/ws/<room_id>` 部分が両 toml で完全一致しているか確認 |
| 対局終局後も R2 に書き込まれない | DO storage の終局イベントが落ちた | Worker のログを `vp exec wrangler tail` で確認 |
| `#TIME_UP` で対局が終わってしまう (シナリオ A 通常実行で) | `BYOYOMI_MS=100` が短すぎ engine 思考が間に合わない | `csa_client` の `[time] margin_msec` を 0 に下げる、または engine option `MaterialLevel = 1` で軽量化 |
