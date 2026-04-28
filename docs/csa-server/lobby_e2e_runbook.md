# Lobby マッチング対局 実機 E2E 運用 Runbook

`rshogi-csa-server-workers` の LobbyDO + GameRoom DO 構成で、`rshogi-csa-client`
2 セッションを連続マッチング対局させる際の運用ハンドブック。staging で実機通電
させた挙動を 1 次資料として、ログの読み方・棋譜の確認・時間管理仕様・マッチング
動作・主要エラーパターンを網羅する。

設計の根拠は [`lobby_design.md`](./lobby_design.md)、deploy 経路は
[`deployment.md`](./deployment.md)、シナリオ列は [`staging-e2e.md`](./staging-e2e.md)
を参照。本 runbook は **「立ち上げて棋譜を確認するまで」** の作業順を実機で示す。

## 1. 前提条件

- staging Worker が `https://rshogi-csa-server-workers-staging.<account>.workers.dev/`
  に deploy 済み (LobbyDO バインディング含む)。`/health` が `rshogi-csa-server-workers v0.1.0`
  を返すこと。
- `target/release/rshogi-csa-client` と `target/release/rshogi-usi` がローカルに
  ビルド済み。`cargo build -p rshogi-csa-client --release && cargo build -p rshogi-usi --release`
  で揃う。
- `vp` ラッパー (`/home/sh11235/.vite-plus/bin/vp`) で `wrangler` が呼べること
  (`wrangler tail` / `wrangler r2 object get` を本 runbook で利用)。
- staging 既定値:

  | 項目 | 値 |
  |---|---|
  | `CLOCK_KIND` | `countdown_msec` |
  | `TOTAL_TIME_MS` | `10000` (10 秒) |
  | `BYOYOMI_MS` | `100` (100ms) |
  | `RECONNECT_GRACE_SECONDS` | `30` |
  | `ALLOW_FLOODGATE_FEATURES` | `true` |
  | `LOBBY_QUEUE_SIZE_LIMIT` | `100` |

## 2. 最短コマンド (1 局完走)

```bash
# 黒番 (alice)
GAME_NAME="op-test-$(date +%s)"
mkdir -p /tmp/lobby-run-black/records
cd /tmp/lobby-run-black && \
  RUST_LOG=info ~/git-repos/rshogi-oss/target/release/rshogi-csa-client \
    --target staging --lobby --game-name "$GAME_NAME" \
    --handle alice --color black \
    --simple-engine --engine ~/git-repos/rshogi-oss/target/release/rshogi-usi \
    --max-games 1 --record-dir ./records 2>&1 | tee black.log

# 別ターミナル / 白番 (bob、同 GAME_NAME を渡す)
mkdir -p /tmp/lobby-run-white/records
cd /tmp/lobby-run-white && \
  RUST_LOG=info ~/git-repos/rshogi-oss/target/release/rshogi-csa-client \
    --target staging --lobby --game-name "$GAME_NAME" \
    --handle bob --color white \
    --simple-engine --engine ~/git-repos/rshogi-oss/target/release/rshogi-usi \
    --max-games 1 --record-dir ./records 2>&1 | tee white.log
```

連続対局は `--max-games 5` のように増やす。`--lobby` 経路は対局終了ごとに
再 LOGIN_LOBBY して queue に戻る。

> 注: `--simple-engine` プリセットは内部既定として `max_games = 1` を設定するが、
> CLI override が後段で適用されるため `--simple-engine --max-games 5` は意図通り
> 5 局回る。

## 3. クライアント側ログの読み方

`csa_client` 標準出力に時系列で以下が出る (`RUST_LOG=info`、各行 `[component]` プレフィクス付き):

```text
[INFO] CSA対局クライアント起動
[INFO] サーバー: wss://...workers.dev/ws/lobby:0 (ID: alice+lobby+black)        ← apply_target_preset 後
[INFO] エンジン: /path/to/rshogi-usi
[INFO] [USI] エンジン準備完了: Shogi Engine 0.1.0                                 ← USI handshake OK
[INFO] [CSA/WS] 接続中: wss://.../ws/lobby
[INFO] [CSA/WS] 接続成功: status=101 Switching Protocols                          ← Origin 検査通過
[INFO] [Lobby] LOGIN_LOBBY 送信: handle=alice game_name=<gname> color=black
[INFO] [Lobby] LOGIN_LOBBY OK (alice OK)、MATCHED 待機                            ← LobbyQueue 登録
[INFO] [Lobby] MATCHED 受信: room_id=lobby-<gname>-<32hex> → host=wss://.../ws/<room_id>
[INFO] [CSA/WS] 接続中: wss://.../ws/lobby-<gname>-<32hex>
[INFO] [CSA/WS] 接続成功: status=101 Switching Protocols                          ← GameRoom DO 接続
[INFO] [CSA] ログイン成功: alice+<gname>+black                                    ← GameRoom LOGIN OK
[INFO] [CSA] 対局待機中...
[INFO] [CSA] 対局情報受信: <game_id> (1手目から) alicevsbob 先手:10000ms+100ms+0ms 後手:10000ms+100ms+0ms
[INFO] [CSA] 対局開始: START:<game_id>
[INFO] [USI] サーバー終局検出、探索中断: #LOSE                                    ← 自分敗北なら
[INFO] [CSA] サーバー終局割り込み: Lose
[INFO] [REC] 棋譜保存: ./records/YYYYMMDD_HHMMSS_alice_vs_bob.csa
[INFO] [REC] SFEN保存: ./records/YYYYMMDD_HHMMSS_alice_vs_bob.sfen
[INFO] 対局 #1 結果: Lose | 通算: 0勝 1敗 0分
[INFO] 最大対局数 (1) に達しました
[INFO] 終了。合計 1 局: 0勝 1敗 0分
```

主要 component:
- `[CSA/WS]`: `transport.rs` の WebSocket 接続層 (TLS / Upgrade)。
- `[Lobby]`: `main.rs::acquire_lobby_match` (LOGIN_LOBBY / MATCHED 受信)。
- `[CSA]`: `protocol.rs` (LOGIN / Game_Summary / 指し手 / 終局)。
- `[USI]`: `engine.rs` (USI ハンドシェイク / go / bestmove)。
- `[REC]`: `record.rs` (CSA / SFEN ファイル書き出し)。

`RUST_LOG=debug` にすると `[CSA] >` (送信) / `[CSA] <` (受信) の行ベース ping
が出る。password はマスクされる。

## 4. サーバ側ログの読み方 (`wrangler tail`)

```bash
cd ~/git-repos/rshogi-oss/crates/rshogi-csa-server-workers && \
  vp exec wrangler tail --config wrangler.staging.toml --format pretty
```

実機通電時に観測される行 (LobbyDO + GameRoom DO の両方):

```text
GET https://.../ws/lobby - Canceled                                       ← 1 件目 LOGIN_LOBBY 接続
  (log) [Lobby] websocket upgrade accepted
GET https://.../ws/lobby - Canceled                                       ← 2 件目接続
  (log) [Lobby] websocket upgrade accepted
Unknown Event - Ok
  (log) [Lobby] LOGIN_LOBBY: handle=alice game_name=<gname> color=Black queue_size=1
Unknown Event - Ok
  (log) [Lobby] LOGIN_LOBBY: handle=bob game_name=<gname> color=White queue_size=2
  (log) [Lobby] MATCHED dispatched: room_id=lobby-<gname>-<32hex> black=alice white=bob (sent_black=true sent_white=true)
Unknown Event - Ok
  (log) [Lobby] queued client closed: handle=alice queue_size=0           ← MATCHED 後 LobbyDO 側 close
  (log) [Lobby] queued client closed: handle=bob queue_size=0
GET https://.../ws/lobby-<gname>-<32hex> - Canceled                       ← GameRoom DO 接続 (黒)
  (log) [GameRoom] websocket upgrade accepted
  (log) [GameRoom] websocket upgrade accepted                             ← (白)
... 対局中の通信は Workers tail には出ない (DO 内 hibernation 経由) ...
Unknown Event - Ok
  (log) [GameRoom] handle_line error: State(InvalidForState { current: "Finished(TimeUp { loser: Black })" })
  (log) [GameRoom] handle_line error: State(InvalidForState { current: "Finished(TimeUp { loser: Black })" })
  (log) [GameRoom] entered grace window: role=White grace_secs=30
  (log) [GameRoom] entered grace window: role=Black grace_secs=30
  (log) [GameRoom] kifu exported to R2 key='2026/04/28/<game_id>.csa'
Alarm @ ...                                                                ← grace 期限 alarm 発火
```

メモ:
- `Unknown Event - Ok` は WebSocket 側のメッセージ受信契機 (Cloudflare の表示上の
  ラベルで、内容は `(log) ...` の各行に出る)。
- `GET .../ws/<path> - Canceled` は WS Upgrade のリクエストログ。`Canceled` は
  HTTP リクエストが WS にアップグレードされたことを意味する (異常ではない)。
- `[GameRoom] handle_line error: State(InvalidForState {...})` は終局後に
  client の cleanup line (`%TORYO` 等) が届いた場合の保護的なエラーで、
  実害なし (対局はすでに `Finished(TimeUp {...})` 状態)。
- `entered grace window: role=<color> grace_secs=30` は再接続猶予枠への遷移ログ。
  `RECONNECT_GRACE_SECONDS=30` 設定下で client 切断時に必ず出る。
- 最終行 `kifu exported to R2 key=...` で棋譜が R2 に書き込まれた key が分かる。

`wrangler tail` を回しっぱなしで対局する場合、3.x 系は `wrangler@4` 推奨警告
が出るが動作上の問題はない (`workspace` 全体で 4 系移行は別 task)。

## 5. 棋譜の確認

### 5.1 ローカルファイル (`csa_client` 側)

`--record-dir ./records` で指定したディレクトリに以下が書き出される:

```
records/
├── 20260428_204721_alice_vs_bob.csa     ← CSA V2 形式 (Floodgate 互換)
└── 20260428_204721_alice_vs_bob.sfen    ← USI SFEN 形式
```

ファイル名は **対局開始時刻 (UTC)** + handle ペア。同 handle ペアの 2 局目以降は
別タイムスタンプで重ならない。

CSA 棋譜の主要セクション:

```text
V2.2
N+alice                                             ← 黒
N-bob                                               ← 白
$EVENT:<game_id>                                    ← client 側のみ (sub §5.2 サーバ側棋譜には出ない)
$START_TIME:2026/04/28 20:47:21
$TIME_LIMIT:0:10+00                                 ← 持ち時間表記
P1-KY-KE-GI-KI-OU-KI-GI-KE-KY                       ← 平手初期局面
... 9 段 ...
+
'* 17 +2726FU -3334FU                               ← floodgate 拡張: 評価値 + PV
+2726FU
T1                                                  ← 消費時間 (秒)
-3334FU
T2
... 中略 ...
%TIME_UP                                            ← 終局理由
```

### 5.2 サーバ側棋譜 (R2 `rshogi-csa-kifu-staging`)

GameRoom DO が終局時に書き出すサーバ確定棋譜。`game_room.rs::export_kifu_to_r2`
経由で `YYYY/MM/DD/<game_id>.csa` キーに保存される。

```bash
cd ~/git-repos/rshogi-oss/crates/rshogi-csa-server-workers && \
  vp exec wrangler r2 object get \
    rshogi-csa-kifu-staging/2026/04/28/<game_id>.csa \
    --config wrangler.staging.toml --file /tmp/server.csa
cat /tmp/server.csa
```

`<game_id>` は wrangler tail の `kifu exported to R2 key=...` 行や client 側の
`対局情報受信: <game_id>` 行から取得する。

サーバ側棋譜は client 側 (`./records/*.csa`) と原則同一 (両者 V2.2 / 同じ手数)
だが、サーバ側はさらに `BEGIN Time` ブロック (Time_Unit / Total_Time / Byoyomi)
が完全形で入る点が違う。

```text
V2.2
N+alice
N-bob
$GAME_ID:<game_id>
$START_TIME:2026/04/28 12:12:56
$END_TIME:2026/04/28 12:13:34                       ← サーバ側のみ
BEGIN Time
Time_Unit:1msec
Total_Time:10000
Byoyomi:100
Least_Time_Per_Move:0
END Time
... (P1〜P9 / 指し手 / %TIME_UP)
```

### 5.3 Floodgate 履歴 (R2 `rshogi-csa-floodgate-history-staging`)

`ALLOW_FLOODGATE_FEATURES = "true"` 環境では終局時に 1 対局 = 1 JSON サマリが
書き出される。

```bash
vp exec wrangler r2 object get \
  rshogi-csa-floodgate-history-staging/floodgate-history/2026/04/28/HHMMSS-<game_id>.json \
  --config wrangler.staging.toml --file /tmp/history.json
cat /tmp/history.json
```

JSON 内容例:

```json
{
  "game_id": "lobby-<gname>-<32hex>-<epoch_ms>",
  "game_name": "<gname>",
  "black": "alice",
  "white": "bob",
  "start_time": "2026-04-28T12:12:56Z",
  "end_time": "2026-04-28T12:13:34Z",
  "result_code": "#TIME_UP",
  "winner": "White"
}
```

`list_recent(N)` 用に `floodgate-history/YYYY/MM/DD/HHMMSS-<game_id>.json` の
day-shard 形式で書き込まれる。検索は当日キーから逆順に走査する設計
(`R2FloodgateHistoryStorage::list_recent`)。

## 6. 終局条件と時間管理

### 6.1 staging の時計設定 (`countdown_msec`)

| 項目 | 値 (staging) |
|---|---|
| Time_Unit | `1msec` |
| Total_Time | `10000` (= 10 秒の本体時間) |
| Byoyomi | `100` (= 100ms の 1 手秒読み) |
| Increment | `0` |
| Least_Time_Per_Move | `0` |

各手の消費時間は `T<sec>` 行で記録される (秒単位)。`Time_Unit:1msec` だが棋譜表記
は秒に丸める CSA 仕様。本体時間 10 秒 + 100ms 秒読みのため、エンジンが 100ms 以内に
指せない手で `%TIME_UP`。本リポ実機実行では **大半の局が `%TIME_UP`** で終局している。
これは「短時間 E2E のための極端設定」で、本番運用 `countdown` (Time_Unit:1sec /
Total_Time:600 / Byoyomi:10) ではこのレートで時間切れにはならない。

### 6.2 終局コードと記録

| 終局トークン | 意味 | history JSON `result_code` | broadcast 通知行 (各 client) | 記録経路 |
|---|---|---|---|---|
| `%TORYO` | 投了 | `#RESIGN` | `#RESIGN` + 勝者 `#WIN` / 敗者 `#LOSE` | client が `engine bestmove resign` で送出 |
| `%KACHI` | 入玉宣言勝ち (24点法成立) | `#JISHOGI` | `#JISHOGI` + 勝者 `#WIN` / 敗者 `#LOSE` | client が `engine bestmove win` で送出 |
| `%CHUDAN` | 中断 (合意中断、winner 無し) | `#ABNORMAL` | `#ABNORMAL` 単独 | サーバ側で `force_abnormal(None)` 経由 |
| (`force_abnormal` 切断経路) | 切断 grace 超過 / 運営権限による強制終了 | `#ABNORMAL` | `#ABNORMAL` + 残存側 `#WIN` / 切断側 `#LOSE` | サーバ側で `force_abnormal(Some(loser))` 経由 |
| `%TIME_UP` | 時間切れ | `#TIME_UP` | `#TIME_UP` + 勝者 `#WIN` / 敗者 `#LOSE` | サーバ側 alarm 発火、`force_time_up` で確定 |
| `%ILLEGAL_MOVE` | 不正手 | `#ILLEGAL_MOVE` | `#ILLEGAL_MOVE` + 勝者 `#WIN` / 敗者 `#LOSE` | サーバ側 `handle_line` の parse エラー |

`#WIN` / `#LOSE` は **broadcast 通知行のみ** に出るコードで、`result_code`
フィールドの値ではない (history JSON の `result_code` は `#RESIGN` / `#JISHOGI` /
`#TIME_UP` / `#ILLEGAL_MOVE` / `#ABNORMAL` のいずれか)。

`#SENNICHITE` / `#OUTE_SENNICHITE` / `#MAX_MOVES` 等の他の終局コードは
`primary_result_code` (`crates/rshogi-csa-server/src/record/kifu.rs`) で
返るが、本 runbook の典型シナリオでは出現しないため割愛。

### 6.3 サーバ側 alarm との関係

GameRoom DO は `state.storage().set_alarm(<deadline_ms>)` で turn deadline alarm を
予約し、到着時に `force_time_up(current_turn)` で時間切れを確定する。staging で
`%TIME_UP` 終局が大量に出るのは、alarm 発火が **wall-clock 時間 (Cloudflare 側)**
で評価されるため、エンジン go の時間枠内でも client → サーバの ws レイテンシ
(数十〜数百 ms) が消費時間に乗るのが効いている。

## 7. マッチング動作

### 7.1 LobbyDO の処理

1. `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` を受信。
2. `<game_name>` 文字種検証 (`[A-Za-z0-9_-]` / 1〜32 文字)。違反は
   `LOGIN_LOBBY:incorrect bad_game_name` で reject + close。
3. queue に enqueue。同 handle の旧エントリは `evict_old` で除去
   (旧 WS attachment も close)。queue 上限超過 (`LOBBY_QUEUE_SIZE_LIMIT`、既定 100)
   は `LOGIN_LOBBY:incorrect queue_full` で reject。
4. `LOGIN_LOBBY:<handle> OK` を返信。
5. **その場で `try_pair`** を呼ぶ (`DirectMatchStrategy`、同一 `game_name` で
   complementary color の **先着 1 ペア**。queue 内 enqueue 順 (= 名前ソート後
   FIFO) で最初に matching 条件を満たす 2 名をペアにする)。
6. ペア成立 → 128 bit hex の room_id 発番 → 両 client に `MATCHED <room_id> <color>`
   送信 → 両 WS close。

### 7.2 マッチング応答時間 (実測、staging)

| ケース | 応答時間 |
|---|---|
| 2 client が時間差なく接続 | ~200ms (server-side 処理 + WS hop) |
| 黒先着 → 白後発 | 黒は white の LOGIN_LOBBY 受信時にペア確定で即時送信 (~20ms) |
| 同 handle 重複 LOGIN | 旧 entry / 旧 WS 共に evict 後に新 entry 登録 (~30ms) |

### 7.3 「即座マッチング」の前提条件

- `<game_name>` が一致していること。違うとペアにならず、サーバ側 queue は TTL 未実装
  のため LobbyDO 内に残り続ける。client 側は MATCHED 受信を `recv_line_blocking(60s)`
  で待つので、60 秒タイムアウト → エラー bail で `Init` 状態に戻り、外側 retry_delay
  経由で再 LOGIN_LOBBY する (shutdown でのみ完全離脱)。
- 黒・白の `<color>` が complementary (一方が `black` で他方が `white`)。
  両方 `black` だとペアにならない (DirectMatchStrategy の仕様)。
- LOGIN_LOBBY のフォーマットが正しいこと (上記 §7.1 step 2 参照)。

## 8. 連続マッチング対局の挙動

`--max-games 5` で実行すると以下のループに入る:

```
[局 #1]
  LOGIN_LOBBY → MATCHED → /ws/<room_id_1> 接続 → 1 局完走
  → records/YYYYMMDD_HHMMSS_alice_vs_bob.csa 書き出し
  → R2 棋譜 (rshogi-csa-kifu-staging) に同 game_id で書き出し
[局 #2]
  再 LOGIN_LOBBY → MATCHED (新 room_id_2) → 1 局完走 → 同様
...
[局 #5]
  → "最大対局数 (5) に達しました" → 終了
```

各局で `room_id` は新発番 (128 bit rand のため衝突しない)。client 側 record_dir
にも 5 ファイル × 2 種 (.csa + .sfen) = 10 ファイルが残る。

LobbyDO は `Hibernation` 中に対局終了後の再 LOGIN_LOBBY を受けるため、`/ws/lobby`
への次の接続は 1 件目より少し遅い (~1 秒、wake-up コスト) ことがある。

## 9. トラブルシューティング

| 症状 | 原因候補 | 対処 |
|---|---|---|
| `[Lobby] LOGIN_LOBBY 送信:` 後に `MATCHED 受信` がいつまでも出ない | 相手 client の `<game_name>` が違う / 接続失敗 / 両者同色 | 両 client の `--game-name` / `--color` を再確認 |
| `LOGIN_LOBBY:incorrect bad_game_name` | `<game_name>` に許可外文字 (`.`, `+`, スペース 等) または長さ違反 | `[A-Za-z0-9_-]` 1〜32 文字に修正 |
| `LOGIN_LOBBY:incorrect queue_full` | LobbyDO queue が `LOBBY_QUEUE_SIZE_LIMIT` (既定 100) を超過 | `wrangler.staging.toml` で値を引き上げて再 deploy、または queue の自然減を待つ |
| `[CSA/WS] 接続失敗: 403 Forbidden Origin` | csa_client 側 `ws_origin` が staging の `WS_ALLOWED_ORIGINS` allowlist に含まれない | `--ws-origin` を allowlist 値に揃えるか、`--ws-origin` を未指定にする (Origin ヘッダを送らないネイティブ経路は allowlist にエントリがあっても素通し) |
| `[CSA] 対局待機中...` のまま MATCHED 後の Game_Summary が来ない | GameRoom DO 側 `handle_login` が reject (例: 同 game_name で 3 人目接続) | `wrangler tail` で `[GameRoom] handle_login` 相当のエラーを確認 |
| 全局 `%TIME_UP` で対局が短い | staging の `BYOYOMI_MS=100` が極端に短い | これは想定挙動。`countdown` モードに切り替えて確認したい場合は `wrangler.staging.toml` の `CLOCK_KIND="countdown"` + `TOTAL_TIME_SEC` / `BYOYOMI_SEC` を設定して再 deploy |
| `[GameRoom] handle_line error: State(InvalidForState ...)` がログに出る | 終局後の cleanup line (`%TORYO` 等) が届いた | 害なし。GameRoom DO は終局状態を保持しており保護的にエラーを返している |
| R2 棋譜 list が空 | 終局していない / 終局でも `KIFU_BUCKET` binding が間違っている | `wrangler.staging.toml` の `[[r2_buckets]] binding = "KIFU_BUCKET" / bucket_name = "rshogi-csa-kifu-staging"` を確認 |
| `floodgate-history` バケットが空 | `ALLOW_FLOODGATE_FEATURES = "false"` (production 既定) | staging では既に `"true"` 設定。production で history を有効化する場合は `[vars]` を更新 |

## 10. 実機セッション例 (記録)

実際に 2026-04-28 の staging 実機で取得した値の参考:

- 1 局完走: `--simple-engine` で 30〜45 秒 (Cloudflare cold-start + 100ms byoyomi)
- 連続 3 局: client 側合計 約 110 秒 (1 局 38 秒 × 3 + handoff 数秒 × 2)
- 全局 `%TIME_UP` 終局 (短時間設定の想定範囲)
- R2 棋譜 + Floodgate history JSON 両方に書き出し確認

ログ・棋譜サンプルは `/tmp/csa-lobby-runs*/` 配下に残るので、必要に応じて
`docs/csa-server/staging-e2e.md` の手順例と組み合わせて参照する。
