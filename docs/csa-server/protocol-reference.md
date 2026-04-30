# rshogi-csa-server プロトコル参照

`rshogi-csa-server` (TCP / Cloudflare Workers 共通の core crate) と、その上に乗る
`rshogi-csa-server-tcp` / `rshogi-csa-server-workers` が話す CSA プロトコル方言の
利用者向けリファレンス。OSS 利用者が「rshogi-oss を CSA client から繋いだとき
何が送れて何が返ってくるか」を実装位置 (`file:line`) 付きで一望できることを目的
にする。

`*` 印は本リポ独自拡張、 `**` 印は本リポ独自拡張のうち CSA v1.2.1 標準互換の
範囲を意図的に逸脱しているもの (Floodgate 系互換のために追加)。

## 1. このドキュメントのスコープ

| 含む | 含まない |
|---|---|
| 標準 CSA コマンド (LOGIN / AGREE / Move / TORYO / KACHI / CHUDAN) の本リポ受理範囲 | プロトコル設計の議事録・歴史的経緯 |
| x1 拡張コマンド (`%%WHO`〜`%%FLOODGATE rating`) と応答 framing | 個別運用環境のパラメタ・URL |
| 本リポ独自拡張 (`Reconnect_Token` / `BEGIN Reconnect_State` / Lobby `MATCHED`) | Floodgate オプトイン gate の運用方法 (別 doc) |
| 各コマンドの実装位置 `file:line` | TCP / Workers のデプロイ手順 (別 doc) |

CSA プロトコル一般仕様や本家 Floodgate 運用は §2 の外部参照に投げ、ここでは
「本リポ実装が受理する語彙と返す語彙」を契約として扱う。

## 2. 外部仕様への参照

本リポ実装は以下の公開仕様 / 互換実装を出発点にしている。標準コマンドの解釈で
本リポ未記載の細部 (例えば `T<sec>` の表現や `Game_Summary` の必須キー順) は
これらの一次ソースを参照すること。

| 種別 | 名称 | 主な用途 |
|---|---|---|
| 公開仕様 | CSA 通信プロトコル ([`computer-shogi.org/protocol`](http://www2.computer-shogi.org/protocol/)) | LOGIN / Game_Summary / 指し手トークンの一次仕様 |
| 互換実装 | Ruby [shogi-server](https://github.com/TadaoYamaoka/shogi-server) | x1 拡張 (`%%WHO` 等) と Floodgate 相当のマッチング実装の挙動リファレンス |
| 互換運用 | Floodgate (`wdoor.c.u-tokyo.ac.jp`) | 接続確認用の代表的な公開サーバ。本リポは独自サーバ実装だが、`%%FLOODGATE history` / `%%FLOODGATE rating` / Lobby マッチング はここの運用慣習に倣う |

## 3. wire format 概観

- 行指向。1 メッセージ = 1 行。受信側は CR/LF 双方を許容し、送信側は LF (`\n`)
  または CR/LF (`\r\n`) を末尾に付ける (実装は [`crates/rshogi-csa-server-tcp/src/transport.rs`](../../crates/rshogi-csa-server-tcp/src/transport.rs) の `TcpTransport`)。
- 1 行のパースは [`parse_command`](../../crates/rshogi-csa-server/src/protocol/command.rs) (`crates/rshogi-csa-server/src/protocol/command.rs:159`)、
  クライアント側送信側の組み立ては同ファイル `serialize_client_command` (L486)。
- 空行 (改行のみ) は keep-alive として扱われる ([`ClientCommand::KeepAlive`](../../crates/rshogi-csa-server/src/protocol/command.rs))。
- サーバー → クライアント方向の応答は CSA 標準応答 (例 `LOGIN:alice OK` / `START:<game_id>`) と、x1 拡張で導入した `##[<TAG>] ... ##[<TAG>] END` の 2 種類が混在する。`##` プレフィックスは「このリポ拡張」、`#` プレフィックス (`#WIN` 等) は CSA 標準終局コード。

## 4. 標準 CSA コマンド (client → server)

すべて [`parse_command`](../../crates/rshogi-csa-server/src/protocol/command.rs) (L159) で受理される。

| 行 | 受理可否 | 備考 |
|---|---|---|
| `LOGIN <name> <password>` | ✅ | 通常モードで対局参加。パスワード保存は shogi-server 互換 (`crates/rshogi-csa-server-tcp/src/auth.rs`) |
| `LOGIN <name> <password> x1` | ✅ | x1 拡張モード。`%%WHO` 等が利用可能になる (`command.rs:201-205`) |
| `LOGIN <name> <password> reconnect:<game_id>+<token>` `**` | ✅ | 再接続経路 (§9.1)。`x1` と排他 (`command.rs:204-225`) |
| `LOGOUT` | ✅ | 余剰トークン拒否 (`command.rs:243`) |
| `AGREE [<game_id>]` | ✅ | `<game_id>` 省略時は `None` (`command.rs:249`) |
| `REJECT [<game_id>]` | ✅ | 同上 (`command.rs:256`) |
| `<sign><from><to><PT>[,T<sec>][,'<comment>]` | ✅ | 指し手。先頭 `+`/`-` で先後判定。`T<sec>` 経過秒、`'<comment>` は Floodgate 拡張コメント (PV 等) (`parse_move`, `command.rs:267`) |
| `%TORYO` / `%KACHI` / `%CHUDAN` | ✅ | 投了 / 入玉宣言 / 中断 (`command.rs:182`) |
| 空行 | ✅ | keep-alive (`command.rs:163`) |

サーバー → クライアント方向の標準応答と本リポでの実装位置:

| 応答 | 意味 | 実装位置 |
|---|---|---|
| `LOGIN:<handle> OK` | 認証成功 | `crates/rshogi-csa-server-tcp/src/server.rs:950` |
| `LOGIN:incorrect [<reason>]` | 認証失敗。`<reason>` は本リポ拡張で `unknown_game_name` / `already_logged_in` / `rate_limited retry_after=<sec>` / `reconnect_rejected` / `reconnect_already_resumed` / `reconnect_aborted` を返す `*` | `server.rs:869-916, 1024, 2736-2811` |
| `START:<game_id>` | 両者 AGREE 後の対局開始通知 | `crates/rshogi-csa-server/src/game/room.rs:369` |
| `REJECT:<game_id>` | どちらかが REJECT した | `server.rs:2194-2195` |
| `<token>,T<sec>` | 1 手分の broadcast (各 client / 観戦者へ送出) | `crates/rshogi-csa-server-tcp/src/server.rs` `parse_move_broadcast` (L2889) |

## 5. x1 拡張コマンド一覧

`LOGIN ... x1` が成立したセッションのみ受理される追加コマンド。すべての応答は
§6 の `##[<TAG>] ... ##[<TAG>] END` 框 (framing) を採用し、persistent socket 上で
クライアントが「END まで読む」契約で安全に framing できる。

実装本体は parse 側が [`parse_x1`](../../crates/rshogi-csa-server/src/protocol/command.rs) (`command.rs:298`)、
応答行生成側が [`crates/rshogi-csa-server/src/protocol/info.rs`](../../crates/rshogi-csa-server/src/protocol/info.rs) と
[`crates/rshogi-csa-server-tcp/src/server.rs`](../../crates/rshogi-csa-server-tcp/src/server.rs) のセッションループ。

| コマンド | 概要 | パース位置 | 応答位置 |
|---|---|---|---|
| `%%WHO` | ログイン中プレイヤ一覧。`##[WHO] <name> <status>` を name 昇順、終端 `##[WHO] END` | `command.rs:305` | `info.rs:52` (`who_lines`) |
| `%%LIST` | アクティブ対局一覧。`##[LIST] <game_id> <black> <white> <game_name> <started_at>` + END | `command.rs:309` | `info.rs:81` (`list_lines`) |
| `%%SHOW <game_id>` | 1 対局のサマリ。未登録は `##[SHOW] NOT_FOUND <game_id>` 後 END | `command.rs:321` | `info.rs:107` (`show_lines`) |
| `%%MONITOR2ON <game_id>` | 観戦購読 (broadcast 受信開始)。応答 `##[MONITOR2] BEGIN <game_id>` / 不在 `##[MONITOR2] NOT_FOUND` / 多重 `##[MONITOR2] BUSY` | `command.rs:327` | `server.rs:1378-1468` |
| `%%MONITOR2OFF <game_id>` | 観戦購読解除。応答 `##[MONITOR2OFF] <game_id>` + END | `command.rs:333` | `server.rs:1515-1518` |
| `%%CHAT <message>` | 観戦中の room へ chat 配信。応答 `##[CHAT] OK <game_id>` / 未観戦時 `##[CHAT] NOT_MONITORING` (broadcast 形式は `##[CHAT] <handle>: <message>`) | `command.rs:339` | `server.rs:1520-1551` |
| `%%VERSION` | 実装名 + バージョン 1 行。`##[VERSION] rshogi-csa-server <CARGO_PKG_VERSION>` | `command.rs:313` | `info.rs:28` (`version_lines`) |
| `%%HELP` | 受理コマンド一覧 (`advertise == accept` で統一) | `command.rs:317` | `info.rs:134` (`help_lines`) |
| `%%SETBUOY <game_name> <moves...> <count>` | Buoy 登録。**admin 権限必須** (`config.admin_handles`)。応答 `##[SETBUOY] OK <buoy> <count>` / `PERMISSION_DENIED` / `ERROR <buoy> <reason>` | `command.rs:342` | `server.rs:1553-1591` |
| `%%DELETEBUOY <game_name>` | Buoy 削除。admin 権限必須。応答 `##[DELETEBUOY] OK/PERMISSION_DENIED/ERROR` | `command.rs:363` | `server.rs:1593-1605` |
| `%%GETBUOYCOUNT <game_name>` | Buoy 残数照会。応答 `##[GETBUOYCOUNT] <buoy> <n>` / `NOT_FOUND` / `ERROR` | `command.rs:369` | `server.rs:1610-1625` |
| `%%FORK <source_game> [<buoy_name>] [<nth_move>]` | 過去対局から buoy を派生。第 2 トークンが数字なら `nth_move` として解釈する曖昧性ルール (`command.rs:120-126`) | `command.rs:375` | `server.rs:1635-1660` |
| `%%FLOODGATE history [N]` `*` | 直近 N 件の Floodgate 対局履歴。`limit` 省略時は frontend 側で 10 件補う | `command.rs:417` | `info.rs:172` (`floodgate_history_lines`) |
| `%%FLOODGATE rating <handle>` `*` | 1 名分の rate / wins / losses / last_game_id / last_modified | `command.rs:432` | `info.rs:222` (`floodgate_rating_lines`) |

`%%HELP` は `advertise == accept` の原則で実装されており、`%%HELP` の 1 行サマリと
本表に列挙したコマンドが常に一致する (`info.rs:134-156` のリストと `parse_x1` の
`match` 分岐がテストで紐付けられている: `info.rs:271-294`)。

## 6. サーバー応答 framing (`##[<TAG>] ... END`)

x1 拡張コマンド応答に共通する framing 規約:

- 応答は 1 行以上の本体 + 終端行 `##[<TAG>] END` で構成する。
- 本体が空 (例: 観戦中対局なし `%%LIST` が 0 件) でも終端行は必ず出る。
- TAG はコマンド名と直接対応させる (`%%WHO` → `##[WHO]`, `%%FLOODGATE history` →
  `##[FLOODGATE] history` と `##[FLOODGATE] history END`)。
- `<TAG>` は ASCII 大文字 + 数字 + 区切り `_`/`空白` のみ。フィールド値に
  ASCII 空白を含めない契約 (例 `FloodgateHistoryEntry` の各フィールド) で行
  framing が壊れないことを `debug_assert!` で担保している (`info.rs:177-189`,
  L229-240)。

クライアント実装の側では「`##[<TAG>] END` まで読む」契約だけで複数行応答を
安全に分節できる。

その他 `##` プレフィックス応答 (上表外の運用通知系):

| 応答 | 用途 | 実装位置 |
|---|---|---|
| `##[NOTICE] server shutting down` `*` | TCP サーバー graceful shutdown 通知 | `server.rs:1219` |
| `##[NOTICE] session evicted by duplicate login` `*` | 重複ログイン時の旧セッション通知 | `server.rs:1243` |
| `##[ERROR] buoy '<name>' exhausted` `*` | Buoy 残数 0 時の起動拒否 | `server.rs:1085` |
| `##[ERROR] scheduled match aborted: ...` `*` | スケジューラ起因の対局中止 | `crates/rshogi-csa-server-tcp/src/scheduler.rs:580` |

## 7. Game_Summary ブロック

CSA v1.2.1 標準 `BEGIN Game_Summary` / `END Game_Summary` の組み立ては
[`crates/rshogi-csa-server/src/protocol/summary.rs`](../../crates/rshogi-csa-server/src/protocol/summary.rs) に集約する。

| 関数 | 用途 | 位置 |
|---|---|---|
| `GameSummaryBuilder::build_for(you)` | 対局者宛て (`Your_Turn:` 付き) | `summary.rs:91` |
| `GameSummaryBuilder::build_for_spectator(black_ms, white_ms)` `*` | 観戦者宛て。`Your_Turn:` を出さず、末尾に `Black_Time_Remaining_Ms:` / `White_Time_Remaining_Ms:` を追加 | `summary.rs:56` |
| `standard_initial_position_block()` | 平手 `BEGIN Position` ... `END Position` | `summary.rs:143` |
| `position_section_from_sfen(sfen)` | 任意 SFEN から Position ブロック | `summary.rs:185` |

`build_for` は CSA v1.2.1 標準項目を以下の順で出す: `Protocol_Version` →
`Protocol_Mode` → `Format` → `Declaration` (任意) → `Game_ID` → `Name+` → `Name-` →
`Your_Turn` → `Rematch_On_Draw` → `To_Move` → `BEGIN Time` ... `END Time` →
`BEGIN Position` ... `END Position` → (本リポ拡張) `Reconnect_Token:` →
`END Game_Summary` (テストで順序固定: `summary.rs:319-349`)。

## 8. 終局メッセージ

[`crates/rshogi-csa-server/src/game/result.rs`](../../crates/rshogi-csa-server/src/game/result.rs) で生成。送信順は **「(a) 終局理由コード → (b) 勝敗コード」** を厳守する。

| `GameResult` | 終局理由行 | 勝者 / 敗者 / 観戦者へ | 実装位置 |
|---|---|---|---|
| `Toryo` (`%TORYO`) | `#RESIGN` | 勝者 `#WIN` / 敗者 `#LOSE` / 観戦 `#WIN` | `result.rs:83` |
| `TimeUp` | `#TIME_UP` | 同上 | `result.rs:84` |
| `IllegalMove` (Generic / Uchifuzume / IllegalKachi) | `#ILLEGAL_MOVE` | 同上 | `result.rs:85` |
| `Kachi` (`%KACHI` 成立) | `#JISHOGI` | 同上 | `result.rs:86` |
| `OuteSennichite` (連続王手千日手) | `#OUTE_SENNICHITE` | 同上 (王手側が敗者) | `result.rs:87` |
| `Sennichite` (通常千日手) | `#SENNICHITE` | All に `#DRAW` | `result.rs:88` |
| `MaxMoves` | `#MAX_MOVES` | All に `#CENSORED` | `result.rs:91` |
| `Abnormal { winner: Some(_) }` | `#ABNORMAL` | 勝敗付きで pair 配信 | `result.rs:94` |
| `Abnormal { winner: None }` | `#ABNORMAL` | All に `#ABNORMAL` のみ | `result.rs:96` |

## 9. 本リポ独自拡張

CSA v1.2.1 標準互換クライアントは未知キー / 未知行を無視できる前提で、すべて
**追記行 / 追記ブロック** として標準フローを壊さない位置に組み込まれる。

### 9.1 再接続 (Reconnect_Token / `BEGIN Reconnect_State`) `**`

対局中に対局者の片方が切断したとき、設定 `RECONNECT_GRACE_SECONDS` の grace 内
で再ログインし対局を引き継げる。

**1. 起点: 対局開始時に Game_Summary 末尾へ拡張行を埋める**

`GameSummaryBuilder::build_for(Color)` (`summary.rs:117-126`) は、`black_reconnect_token`
/ `white_reconnect_token` が `Some` の場合のみ、`END Position` の後・
`END Game_Summary` の直前に以下を出す。標準項目の後の追記なので CSA v1.2.1
互換クライアントは無視できる:

```
Reconnect_Token:<16 hex>
```

**2. クライアント側の再ログイン**

切断側クライアントは新しい TCP セッションで以下を送る (`command.rs:201-225`):

```
LOGIN <handle> <password> reconnect:<game_id>+<token>
```

`x1` モードフラグとは排他。`<game_id>` は Game_Summary の `Game_ID:` で受け取った
値、`<token>` は `Reconnect_Token:` で受け取った値。

**3. サーバー側の判定と応答**

[`handle_reconnect_request`](../../crates/rshogi-csa-server-tcp/src/server.rs) (`server.rs:2712`) が grace 中の対局を探索し、handle / color /
token がすべて一致した場合のみ受理する。

| 判定 | 応答 | 補足 |
|---|---|---|
| token 一致 | `LOGIN:<handle> OK` → resume message → transport handoff | `server.rs:2780-2814` |
| game_id 不在 / handle・color 不一致 / token 不一致 | `LOGIN:incorrect reconnect_rejected` | side-channel 漏洩防止のため理由を統合 (`server.rs:2700-2761`) |
| 既に他経路で再接続済み | `LOGIN:incorrect reconnect_already_resumed` | `server.rs:2768-2776` |
| game loop 側が deadline 超過済 | `LOGIN:incorrect reconnect_aborted` | `server.rs:2811` |

**4. resume message のフォーマット**

`build_resume_message` (`server.rs:2824-2846`) が以下を 1 つの multi-line メッセージで送出する:

```
BEGIN Game_Summary
... (切断時点の position_section、Reconnect_Token: 拡張行を含む)
END Game_Summary
BEGIN Reconnect_State
Current_Turn:<+|->
Black_Time_Remaining_Ms:<u64>
White_Time_Remaining_Ms:<u64>
Last_Move:<csa-move>      ← 直前の指し手がある場合のみ
END Reconnect_State
```

`BEGIN Reconnect_State` ... `END Reconnect_State` は本リポ独自で、CSA 標準には
存在しない。Workers 側にも同形式の実装がある (`crates/rshogi-csa-server-workers/src/reconnect.rs:128-153`)。

### 9.2 Lobby マッチング (`MATCHED <room_id> <color>`) `**`

Workers 限定の独自経路。CSA 標準の LOGIN とは別系統 (`/ws/lobby` route) で、
2 client が `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` を送り合う
ことでペアリング → `room_id` 発番 → `MATCHED <room_id> <color>` 通知 → 通常の
GameRoom DO への接続、というフローを取る。

| 行 | 役割 | 実装位置 |
|---|---|---|
| `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` | queue 追加 | `crates/rshogi-csa-server-workers/src/lobby_protocol.rs:70` |
| `LOGOUT_LOBBY` | queue 離脱 | `crates/rshogi-csa-server-workers/src/lobby.rs` |
| `LOBBY_PING` / `LOBBY_PONG` | DO Hibernation 復帰時の双方向 keep-alive | 同上 |
| `LOGIN_LOBBY:<handle> OK` | queue 登録成功 | `lobby_protocol.rs:231` |
| `LOGIN_LOBBY:incorrect <reason>` | 登録失敗 (`reason` は `LoginLobbyError::reason` 参照) | `lobby_protocol.rs:46-57, 236` |
| `MATCHED <room_id> <color>` | ペアリング成立。`<room_id>` は `lobby-<game_name>-<32hex>` | `lobby_protocol.rs:222` |

詳細設計は [`lobby_design.md`](lobby_design.md)、運用 runbook は
[`lobby_e2e_runbook.md`](lobby_e2e_runbook.md) を参照。

### 9.3 Floodgate オプトイン gate

`%%FLOODGATE history` / `%%FLOODGATE rating` 等の Floodgate 運用機能は、起動時の
opt-in flag (`--allow-floodgate-features` / 環境変数) なしには有効化されない。
詳細は本ディレクトリ別 doc (Floodgate gate 仕様) を参照。

## 10. 関連 doc

実装位置と運用情報は本 doc では扱わない。以下を参照:

- [`README.md`](README.md) - 本ディレクトリの索引
- [`deployment.md`](deployment.md) - Cloudflare Workers の構築 / 運用 runbook
- [`lobby_design.md`](lobby_design.md) - LobbyDO の詳細設計
- [`lobby_e2e_runbook.md`](lobby_e2e_runbook.md) - Lobby マッチングの実機 E2E 運用
- [`staging-e2e.md`](staging-e2e.md) - Workers staging 環境での実機対局シナリオ
- [`viewer_access_control.md`](viewer_access_control.md) - viewer / spectate API の access control 運用
- [`../csa-client.md`](../csa-client.md) - CSA client (`csa_client`) の利用方法
