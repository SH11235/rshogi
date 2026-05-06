# rshogi-csa-server プロトコル参照

`rshogi-csa-server` (TCP / Cloudflare Workers 共通の core crate) と、その上に乗る
`rshogi-csa-server-tcp` / `rshogi-csa-server-workers` が話す CSA プロトコル方言の
利用者向けリファレンス。OSS 利用者が「rshogi-oss を CSA client から繋いだとき
何が送れて何が返ってくるか」を実装位置 (file + symbol 名) 付きで一望できることを
目的にする。

実装位置は **行番号ではなく symbol 名** (`file::function` / `file::Type::method`
/ `file::module`) で示す。行番号は code 編集で陳腐化するため記載しない。
symbol 名から行を引きたいときは `rg "fn parse_command\("` などで grep する。

`*` 印は本リポ独自拡張、 `**` 印は本リポ独自拡張のうち CSA v1.2.1 標準互換の
範囲を意図的に逸脱しているもの (Floodgate 系互換のために追加)。

## 1. このドキュメントのスコープ

| 含む | 含まない |
|---|---|
| 標準 CSA コマンド (LOGIN / AGREE / Move / TORYO / KACHI / CHUDAN) の本リポ受理範囲 | プロトコル設計の議事録・歴史的経緯 |
| x1 拡張コマンド (`%%WHO`〜`%%FLOODGATE rating`) と応答 framing | 個別運用環境のパラメタ・URL |
| 本リポ独自拡張 (`Reconnect_Token` / `BEGIN Reconnect_State` / Lobby `MATCHED`) | Floodgate オプトイン gate の運用方法 (別 doc) |
| 各コマンドの実装位置 (file + symbol 名) | TCP / Workers のデプロイ手順 (別 doc) |

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

- 行指向。1 メッセージ = 1 行。受信側は CR/LF 双方を許容する。送信側の末尾改行
  は frontend で異なる:
  - **TCP**: CRLF (`\r\n`) を付ける ([`crates/rshogi-csa-server-tcp/src/transport.rs`](../../crates/rshogi-csa-server-tcp/src/transport.rs) の
    `TcpTransport::send_line`)。
  - **Workers**: LF (`\n`) のみを付ける (`crates/rshogi-csa-server-workers/src/game_room.rs::send_line`、
    `crates/rshogi-csa-server-workers/src/lobby.rs::send_line` の薄いラッパ)。
- 1 行のパースは [`crates/rshogi-csa-server/src/protocol/command.rs`](../../crates/rshogi-csa-server/src/protocol/command.rs) の `parse_command`、
  クライアント側送信側の組み立ては同ファイルの `serialize_client_command`。
  両者は roundtrip プロパティ `parse_command(serialize(c)) == c` を主要バリアント
  全件についてテストで担保する (`command.rs::tests::parse_then_serialize_then_parse_is_stable_for_all_variants`)。
- 空行 (改行のみ) は keep-alive (`ClientCommand::KeepAlive`)。
- サーバー → クライアント方向の応答は CSA 標準応答 (例 `LOGIN:alice OK` /
  `START:<game_id>`) と、x1 拡張で導入した `##[<TAG>] ... ##[<TAG>] END` の 2
  種類が混在する。`##` プレフィックスは本リポ拡張、`#` プレフィックス (`#WIN`
  等) は CSA 標準終局コード。

## 4. 標準 CSA コマンド (client → server)

すべて `command.rs::parse_command` で受理される。

| 行 | 受理可否 | 備考 |
|---|---|---|
| `LOGIN <name> <password>` | ✅ | 通常モードで対局参加。パスワード保存は shogi-server 互換 ([`crates/rshogi-csa-server-tcp/src/auth.rs`](../../crates/rshogi-csa-server-tcp/src/auth.rs)) |
| `LOGIN <name> <password> x1` | ✅ | x1 拡張モード要求 (`command.rs::parse_command` 内の x1 トークン分岐)。**TCP** ではこのフラグが立ったセッションのみ `%%WHO` / `%%LIST` 等の global query 系を受理する (TCP `server.rs::run_waiter`)。**Workers** は `x1` フラグ自体を保存・参照しないため、フラグ有無に関わらず global query 系は配線されない (詳細は §5) |
| `LOGIN <name> <password> reconnect:<game_id>+<token>` `**` | ✅ | 再接続経路 (§9.1)。`x1` と排他 (同じく `parse_command` の同分岐) |
| `LOGOUT` | ✅ | 余剰トークン拒否 |
| `AGREE [<game_id>]` | ✅ | `<game_id>` 省略時は `None` |
| `REJECT [<game_id>]` | ✅ | 同上 |
| `<sign><from><to><PT>[,T<sec>][,'<comment>]` | ✅ | 指し手。先頭 `+`/`-` で先後判定。`'<comment>` は Floodgate 拡張コメント (PV 等)。**`T<sec>` は CSA 互換のため受理するがサーバー時計には反映されない**: 経過時間は `crates/rshogi-csa-server/src/game/room.rs::GameRoom::handle_move` がサーバ側 `now_ms - move_started_at` から計算する。`command.rs::parse_move` は `<token>` と `'<comment>` だけを抽出する |
| `%TORYO` / `%KACHI` / `%CHUDAN` | ✅ | 投了 / 入玉宣言 / 中断 |
| 空行 | ✅ | keep-alive (`ClientCommand::KeepAlive`) |

サーバー → クライアント方向の標準応答と本リポでの実装位置 (関数 / 型名で示す):

| 応答 | 意味 | 生成箇所 |
|---|---|---|
| `LOGIN:<echo> OK` | 認証成功 (新規対局参加経路)。`<echo>` は **TCP では bare `<handle>`** (`crates/rshogi-csa-server-tcp/src/server.rs::handle_connection` の LOGIN 成功応答送出)、**Workers では LOGIN 行で受け取った `<handle>+<game_name>+<color>` を raw 入力のまま echo** する ([`crates/rshogi-csa-server-workers/src/session_state.rs`](../../crates/rshogi-csa-server-workers/src/session_state.rs) の `LoginReply::Ok::to_line` を `crates/rshogi-csa-server-workers/src/game_room.rs::GameRoom::handle_login` から呼ぶ)。再接続経路の echo 規則は §9.1 を参照 (Workers のみ色トークンが正規化される) | TCP `server.rs::handle_connection` / Workers `session_state.rs::LoginReply` |
| `LOGIN:incorrect [<reason>]` | 認証失敗。`<reason>` は本リポ拡張で `unknown_game_name` / `already_logged_in` / `rate_limited retry_after=<sec>` / `reconnect_rejected` / `reconnect_already_resumed` / `reconnect_aborted` を返す `*` | TCP `server.rs::handle_connection` の各拒否経路 (handle 解析失敗 / `parse_handle` 失敗 / `clock_presets` 不一致) と再接続経路 `server.rs::handle_reconnect_request` |
| `START:<game_id>` | 両者 AGREE 後の対局開始通知 | `crates/rshogi-csa-server/src/game/room.rs::GameRoom::handle_agree` |
| `REJECT:<game_id>` | どちらかが REJECT した | TCP `server.rs::drive_game_inner` の AGREE 結果が false の経路 (`server.rs::wait_both_agree` の戻り値で分岐) |
| `<token>,T<sec>` | 1 手分の broadcast (各 client / 観戦者へ送出)。`T<sec>` 値はサーバー側 `room.rs` で計算した経過秒 | `room.rs::GameRoom::handle_move` (broadcast 行作成)、TCP `server.rs::parse_move_broadcast` (受信側ヘルパ) |

## 5. x1 拡張コマンド一覧

CSA 標準を超えた `%%` 系拡張コマンド。受理条件は frontend で異なる:

- **TCP**: `LOGIN ... x1` が成立したセッションのみが受理対象。`crates/rshogi-csa-server-tcp/src/server.rs::run_waiter` が
  非 x1 waiter で `%%` 系入力を切断扱いにする。
- **Workers**: `x1` フラグを保存・確認せず、`crates/rshogi-csa-server-workers/src/game_room.rs` の
  `GameRoom::handle_player_control_command` / `GameRoom::handle_spectator_line` 経路が
  `parse_command` の結果をそのまま処理する。クライアントは `LOGIN ... x1` を送らなくても
  下表の Workers 対応コマンドを利用できる。

応答 framing は frontend 共通で **`%%VERSION` を除く** すべての応答が §6 の
`##[<TAG>] ... ##[<TAG>] END` 契約に従う。`%%VERSION` だけは 1 行応答
(`##[VERSION] <impl> <ver>`) で `END` 終端行を持たない (`info.rs::version_lines`
が 1 行だけ返す) ため、クライアントは `%%VERSION` への応答を 1 行読みで完結
させること。

実装本体は parse 側が `command.rs::parse_x1`、応答行生成側が
[`crates/rshogi-csa-server/src/protocol/info.rs`](../../crates/rshogi-csa-server/src/protocol/info.rs) と各 frontend のセッションループ
([`crates/rshogi-csa-server-tcp/src/server.rs`](../../crates/rshogi-csa-server-tcp/src/server.rs)、[`crates/rshogi-csa-server-workers/src/game_room.rs`](../../crates/rshogi-csa-server-workers/src/game_room.rs))。

**frontend 対応一覧**: x1 コマンドの parse 自体は core crate に集約 (上記
`parse_x1`) されているが、**実際にどのコマンドに応答するかは frontend ごとに
独立**。Workers は対局 1 室に閉じた DO アーキテクチャのため、global query 系
(`%%WHO` / `%%LIST` / `%%SHOW` / `%%VERSION` / `%%HELP` / `%%FLOODGATE ...`) は
配線していない。

| コマンド | 概要 | TCP | Workers | 応答行生成 |
|---|---|---|---|---|
| `%%WHO` | ログイン中プレイヤ一覧。`##[WHO] <name> <status>` を name 昇順、終端 `##[WHO] END` | ✅ | ❌ | `info.rs::who_lines` |
| `%%LIST` | アクティブ対局一覧。`##[LIST] <game_id> <black> <white> <game_name> <started_at>` + END | ✅ | ❌ | `info.rs::list_lines` |
| `%%SHOW <game_id>` | 1 対局のサマリ。未登録は `##[SHOW] NOT_FOUND <game_id>` 後 END | ✅ | ❌ | `info.rs::show_lines` |
| `%%MONITOR2ON <game_id>` | 観戦購読 (broadcast 受信開始)。応答 `##[MONITOR2] BEGIN <id>` / 不在 `##[MONITOR2] NOT_FOUND <game_id>` / 多重 `##[MONITOR2] BUSY <game_id>`。`<id>` は **TCP では要求された `<game_id>`**、**Workers では `monitor_id` (active_game_id があればそれ、無ければ `room_id`)** が入る | ✅ | ✅ (spectator 経路) | TCP `server.rs` の `ClientCommand::Monitor2On` arm / Workers `game_room.rs::GameRoom::handle_spectator_line` の `Monitor2On` arm |
| `%%MONITOR2OFF <game_id>` | 観戦購読解除。応答 `##[MONITOR2OFF] <id>` + END (`<id>` は §MONITOR2ON と同じ規則。TCP は `<game_id>`、Workers は `monitor_id`)。Workers では未登録 `<game_id>` を渡された場合 `##[MONITOR2OFF] NOT_FOUND <requested>` + END で返す経路がある | ✅ | ✅ (spectator 経路) | TCP `server.rs` の `Monitor2Off` arm / Workers `game_room.rs::GameRoom::handle_spectator_line` の `Monitor2Off` arm |
| `%%CHAT <message>` | room へ chat 配信。応答 `##[CHAT] OK <game_id>` / 未観戦時 `##[CHAT] NOT_MONITORING` (broadcast 形式は `##[CHAT] <handle>: <message>`) | ✅ | ✅ (player + spectator) | TCP `server.rs` の `Chat` arm / Workers `game_room.rs` の `Chat` arm (player + spectator 経路) |
| `%%VERSION` | 実装名 + バージョン 1 行。`##[VERSION] rshogi-csa-server <CARGO_PKG_VERSION>`。**他の x1 応答と異なり END 終端行なし** (§6 の例外) | ✅ | ❌ | `info.rs::version_lines` |
| `%%HELP` | 受理コマンド一覧 (`advertise == accept` で統一) | ✅ | ❌ | `info.rs::help_lines` |
| `%%SETBUOY <game_name> <moves...> <count>` | Buoy 登録。**admin 権限必須** (`config.admin_handles`)。応答 `##[SETBUOY] OK <buoy> <count>` / `PERMISSION_DENIED` / `ERROR <buoy> <reason>` | ✅ | ✅ (player 経路) | TCP `server.rs` の `SetBuoy` arm / Workers `game_room.rs::GameRoom::handle_player_control_command` の `SetBuoy` arm |
| `%%DELETEBUOY <game_name>` | Buoy 削除。admin 権限必須。応答 `##[DELETEBUOY] OK/PERMISSION_DENIED/ERROR` | ✅ | ✅ (player 経路) | TCP `server.rs` の `DeleteBuoy` arm / Workers `game_room.rs` の `DeleteBuoy` arm |
| `%%GETBUOYCOUNT <game_name>` | Buoy 残数照会。応答 `##[GETBUOYCOUNT] <buoy> <n>` / `NOT_FOUND` / `ERROR` | ✅ | ✅ (player 経路) | TCP `server.rs` の `GetBuoyCount` arm / Workers `game_room.rs` の `GetBuoyCount` arm |
| `%%FORK <source_game> [<buoy_name>] [<nth_move>]` | 過去対局から buoy を派生。第 2 トークンが数字なら `nth_move` として解釈する曖昧性ルール (`command.rs::ClientCommand::Fork` の doc コメント参照) | ✅ | ✅ (player 経路) | TCP `server.rs` の `Fork` arm / Workers `game_room.rs` の `Fork` arm |
| `%%FLOODGATE history [N]` `*` | 直近 N 件の Floodgate 対局履歴。`limit` 省略時は frontend 側で 10 件補う | ✅ | ❌ | `info.rs::floodgate_history_lines` |
| `%%FLOODGATE rating <handle>` `*` | 1 名分の rate / wins / losses / last_game_id / last_modified | ✅ | ❌ | `info.rs::floodgate_rating_lines` |

`%%HELP` は `advertise == accept` の原則で実装されており、`%%HELP` の 1 行サマリと
本表に列挙したコマンドが常に一致する (`info.rs::help_lines` のリストと
`command.rs::parse_x1` の `match` 分岐が `info.rs::tests::help_lines_cover_currently_wired_commands`
で紐付けられている)。なお `%%HELP` は TCP frontend のみ応答するため、Workers では
`info::help_lines` の advertise list を直接の wire 契約として扱わないこと。

## 6. サーバー応答 framing

x1 拡張コマンド応答に共通する framing 規約:

- 応答は 1 行以上の本体 + 終端行で構成する。
- 本体が空 (例: 観戦中対局なし `%%LIST` が 0 件) でも終端行は必ず出る。
- 各応答は **framing key** を持ち、終端行は「framing key に ASCII 空白 + `END`
  を付けた行」で表現する。framing key は通常 `##[<TAG>]` だが、`%%FLOODGATE`
  系のみ固定接尾語彙 (`history` / `rating`) を伴い `##[FLOODGATE] history` /
  `##[FLOODGATE] rating` 全体が framing key として 1 セットで動く。

| コマンド | framing key | 終端行 |
|---|---|---|
| `%%WHO` / `%%LIST` / `%%SHOW` / `%%HELP` 等 | `##[<TAG>]` | `##[<TAG>] END` |
| `%%MONITOR2ON` (応答 tag は `MONITOR2`) | `##[MONITOR2]` | `##[MONITOR2] END` |
| `%%MONITOR2OFF` | `##[MONITOR2OFF]` | `##[MONITOR2OFF] END` |
| `%%FLOODGATE history [N]` | `##[FLOODGATE] history` | `##[FLOODGATE] history END` |
| `%%FLOODGATE rating <handle>` | `##[FLOODGATE] rating` | `##[FLOODGATE] rating END` |

`<TAG>` (= `##[ ... ]` の中身) は ASCII 大文字 + 数字 + `_` のみ。フィールド値
に ASCII 空白を含めない契約 (例 `FloodgateHistoryEntry` の各フィールド) で行
framing が壊れないことを `info.rs::floodgate_history_lines` /
`info.rs::floodgate_rating_lines` の `debug_assert!` で担保している。

**例外**: `%%VERSION` のみ単行応答 (`##[VERSION] <impl> <ver>`) で終端行を
持たない (`info.rs::version_lines` が 1 行だけ返す)。これは Cargo.toml バージョンを
1 行で返すだけの軽量照会で、フィールド構造を持たないためフレーミングを省略
している。クライアントは `%%VERSION` の応答を 1 行読みで完結させ、その他の x1
コマンドは上記 framing key の終端行まで読むことで複数行応答を安全に分節できる。

その他 `##` プレフィックス応答 (上表外の運用通知系):

| 応答 | 用途 | 生成箇所 |
|---|---|---|
| `##[NOTICE] server shutting down` `*` | TCP サーバー graceful shutdown 通知 | TCP `server.rs` の shutdown 経路 |
| `##[NOTICE] session evicted by duplicate login` `*` | 重複ログイン時の旧セッション通知 | TCP `server.rs` の duplicate login 経路 |
| `##[ERROR] buoy '<name>' exhausted` `*` | Buoy 残数 0 時の起動拒否 | TCP `server.rs` の buoy 起動経路 |
| `##[ERROR] scheduled match aborted: ...` `*` | スケジューラ起因の対局中止 | [`crates/rshogi-csa-server-tcp/src/scheduler.rs`](../../crates/rshogi-csa-server-tcp/src/scheduler.rs) |

## 7. Game_Summary ブロック

CSA v1.2.1 標準 `BEGIN Game_Summary` / `END Game_Summary` の組み立ては
[`crates/rshogi-csa-server/src/protocol/summary.rs`](../../crates/rshogi-csa-server/src/protocol/summary.rs) に集約する。

| 関数 | 用途 |
|---|---|
| `summary.rs::GameSummaryBuilder::build_for(you)` | 対局者宛て (`Your_Turn:` 付き) |
| `summary.rs::GameSummaryBuilder::build_for_spectator(black_ms, white_ms)` `*` | 観戦者宛て。`Your_Turn:` を出さず、末尾に `Black_Time_Remaining_Ms:` / `White_Time_Remaining_Ms:` を追加 |
| `summary.rs::standard_initial_position_block` | 平手 `BEGIN Position` ... `END Position` |
| `summary.rs::position_section_from_sfen` | 任意 SFEN から Position ブロック |

`build_for` は CSA v1.2.1 標準項目を以下の順で出す: `Protocol_Version` →
`Protocol_Mode` → `Format` → `Declaration` (任意) → `Game_ID` → `Name+` → `Name-` →
`Your_Turn` → `Rematch_On_Draw` → `To_Move` → `BEGIN Time` ... `END Time` →
`BEGIN Position` ... `END Position` → (本リポ拡張) `Reconnect_Token:` →
`END Game_Summary`。順序は `summary.rs::tests::build_for_includes_required_csa_fields_in_order`
で固定されている。

## 8. 終局メッセージ

[`crates/rshogi-csa-server/src/game/result.rs`](../../crates/rshogi-csa-server/src/game/result.rs) で生成。送信順は **「(a) 終局理由コード →
(b) 勝敗コード」** を厳守する。マッピングは `result.rs::GameResult::server_messages`
で定義:

| `GameResult` バリアント | 終局理由行 | 勝者 / 敗者 / 観戦者へ |
|---|---|---|
| `Toryo` (`%TORYO`) | `#RESIGN` | 勝者 `#WIN` / 敗者 `#LOSE` / 観戦 `#WIN` |
| `TimeUp` | `#TIME_UP` | 同上 |
| `IllegalMove` (Generic / Uchifuzume / IllegalKachi) | `#ILLEGAL_MOVE` | 同上 |
| `Kachi` (`%KACHI` 成立) | `#JISHOGI` | 同上 |
| `OuteSennichite` (連続王手千日手) | `#OUTE_SENNICHITE` | 同上 (王手側が敗者) |
| `Sennichite` (通常千日手) | `#SENNICHITE` | All に `#DRAW` |
| `MaxMoves` | `#MAX_MOVES` | All に `#CENSORED` |
| `Abnormal { winner: Some(_) }` | `#ABNORMAL` | 勝敗付きで pair 配信 |
| `Abnormal { winner: None }` | `#ABNORMAL` | All に `#ABNORMAL` のみ |

`result.rs::pair_win_lose` が「勝者・敗者・観戦者」3 宛先への 2 行 (理由 + 勝敗)
組み立てを共通化している。

## 9. 本リポ独自拡張

CSA v1.2.1 標準互換クライアントは未知キー / 未知行を無視できる前提で、すべて
**追記行 / 追記ブロック** として標準フローを壊さない位置に組み込まれる。

### 9.1 再接続 (Reconnect_Token / `BEGIN Reconnect_State`) `**`

対局中に対局者の片方が切断したとき、設定 `RECONNECT_GRACE_SECONDS` の grace 内
で再ログインし対局を引き継げる。

**1. 起点: 対局開始時に Game_Summary 末尾へ拡張行を埋める**

`summary.rs::GameSummaryBuilder::build_for` は、`black_reconnect_token` /
`white_reconnect_token` が `Some` の場合のみ、`END Position` の後・
`END Game_Summary` の直前に以下を出す。標準項目の後の追記なので CSA v1.2.1
互換クライアントは無視できる:

```
Reconnect_Token:<32 hex>
```

`<32 hex>` は `[0-9a-f]` で固定 32 文字 (128 bit 乱数の lowercase hex 表現)。
`crates/rshogi-csa-server/src/types.rs::ReconnectToken::generate` が `rand::random()`
で 16 byte → 32 hex に展開する。クライアントは値を切り詰めず原文のまま保存・
送信すること。

**2. クライアント側の再ログイン**

切断側クライアントは新しい TCP セッションで以下を送る:

```
LOGIN <handle>+<game_name>+<color> <password> reconnect:<game_id>+<token>
```

`<handle>+<game_name>+<color>` は通常 LOGIN と同じ
`crates/rshogi-csa-server-tcp/src/server.rs::parse_handle` を通すため、再接続
要求でも省略不可。bare `<handle>` を送ると `reconnect:` トークンを伴っていても
`LOGIN:incorrect` で拒否される。`x1` モードフラグとは排他。`<game_id>` は
Game_Summary の `Game_ID:` で受け取った値、`<token>` は `Reconnect_Token:` で
受け取った 32 文字。

**3. サーバー側の判定と応答**

TCP は `crates/rshogi-csa-server-tcp/src/server.rs::handle_reconnect_request`、
Workers は `crates/rshogi-csa-server-workers/src/game_room.rs::GameRoom::handle_reconnect_request`
が grace 中の対局を探索し、handle / color / token がすべて一致した場合のみ
受理する。

| 判定 | 応答 |
|---|---|
| token 一致 | `LOGIN:<echo> OK` → resume message → transport handoff。TCP は bare handle (§4 と同様)、Workers は `<handle>+<game_name>+<color>` 形式だが**色トークンは `color_to_str` で正規化** (`black` / `white`) されるため、再接続時 LOGIN 行で `b` / `sente` 等の alias を送ってもサーバー応答は `black` で返る。新規 LOGIN 経路 (§4) は raw 入力をそのまま echo するので、ここだけ挙動が異なることに注意 |
| game_id 不在 / handle・color 不一致 / token 不一致 | `LOGIN:incorrect reconnect_rejected` (side-channel 漏洩防止のため理由を統合) |
| 既に他経路で再接続済み | `LOGIN:incorrect reconnect_already_resumed` |
| game loop 側が deadline 超過済 | `LOGIN:incorrect reconnect_aborted` |

**4. resume message のフォーマット**

`server.rs::build_resume_message` (TCP) と `crates/rshogi-csa-server-workers/src/reconnect.rs::build_resume_message`
(Workers) が以下を 1 つの multi-line メッセージで送出する:

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
存在しない。

### 9.2 Lobby マッチング (`MATCHED <room_id> <color>`) `**`

Workers 限定の独自経路。CSA 標準の LOGIN とは別系統 (`/ws/lobby` route) で、
2 client が `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` を送り合う
ことでペアリング → `room_id` 発番 → `MATCHED <room_id> <color>` 通知 → 通常の
GameRoom DO への接続、というフローを取る。

| 行 | 役割 | 生成 / 受理箇所 |
|---|---|---|
| `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` | queue 追加 | [`crates/rshogi-csa-server-workers/src/lobby_protocol.rs`](../../crates/rshogi-csa-server-workers/src/lobby_protocol.rs) の `parse_login_lobby` |
| `LOGOUT_LOBBY` | queue 離脱 | [`crates/rshogi-csa-server-workers/src/lobby.rs`](../../crates/rshogi-csa-server-workers/src/lobby.rs) の `LobbyDO::handle_queued_line` (`LOGOUT_LOBBY` arm) |
| `LOBBY_PONG` | client → server。受信のみ実装 (queue 滞在中の no-op)。サーバーからの `LOBBY_PING` 送出と PONG 応答処理は未実装 | `lobby.rs::LobbyDO::handle_queued_line` (`LOBBY_PONG` arm) |
| `LOGIN_LOBBY:<handle> OK` | queue 登録成功 | `lobby_protocol.rs::build_login_ok_line` |
| `LOGIN_LOBBY:incorrect <reason>` | 登録失敗 (`reason` は `lobby_protocol.rs::LoginLobbyError::reason` 参照) | `lobby_protocol.rs::build_login_incorrect_line` |
| `MATCHED <room_id> <color>` | ペアリング成立。`<room_id>` は `lobby-<game_name>-<32hex>` (`lobby_protocol.rs::build_room_id`) | `lobby_protocol.rs::build_matched_line` |

詳細設計は [`lobby_design.md`](lobby_design.md)、運用 runbook は
[`lobby_e2e_runbook.md`](lobby_e2e_runbook.md) を参照。

### 9.3 Floodgate オプトイン gate

opt-in flag (`--allow-floodgate-features` / 環境変数) は **コマンドそのもの**
ではなく **起動時の構成 (永続 rates / history / scheduler / 切断敗北確定など)
の有効化** を gate する。

具体的な振る舞い:

- `%%FLOODGATE rating <handle>` は常に受理され、`rate_storage.load()` の結果を
  そのまま返す (TCP `server.rs` の `FloodgateRating` arm)。永続 rates が wire
  されていなければ `NOT_FOUND` 応答に倒れる。
- `%%FLOODGATE history [N]` も常に受理される (TCP `server.rs` の `FloodgateHistory`
  arm)。`history_storage` 未配線時は `##[FLOODGATE] history ERROR not_configured`
  を返す。
- opt-in を伴う構成 (`JsonlFloodgateHistoryStorage` の起動・スケジューラ起動・
  切断敗北確定 など) を要求した状態で `allow_floodgate_features=false` のまま
  起動すると、TCP `server.rs::prepare_runtime` が `Err` を返してプロセス終了する。

opt-in flag が gate するフィールド集合 (`FloodgateFeatureIntent`) と検証
ロジック (`validate_floodgate_feature_gate`) の一次ソースは core crate の
[`crates/rshogi-csa-server/src/config.rs`](../../crates/rshogi-csa-server/src/config.rs)。frontend ごとに「構成 → 要求集合
(intent)」を導出する経路は別物で、TCP は `crates/rshogi-csa-server-tcp/src/server.rs::floodgate_intent_from_config`
を 1 か所に集約、Workers は env 解析時にインラインで `FloodgateFeatureIntent`
を組み立てる (`crates/rshogi-csa-server-workers/src/game_room.rs::resolve_reconnect_grace`
/ `resolve_floodgate_history_storage`、[`crates/rshogi-csa-server-workers/src/games_index.rs`](../../crates/rshogi-csa-server-workers/src/games_index.rs))。

### 9.4 Clock 拡張 (`Time_Unit:1msec` / `countdown_msec`) `**`

本リポ独自拡張の clock variant `countdown_msec` は `Time_Unit:1msec` を
`Game_Summary` に出力する。本家 Floodgate には存在しない 1ms 粒度の秒読み
単位で、staging 環境で短時間 E2E (10s + 100ms 等) を回すために導入。
production は本家 Floodgate 互換の `countdown` (`Time_Unit:1sec`) を既定とし、
`Time_Unit:1msec` を出さない。

実装位置: `crates/rshogi-csa-server/src/game/clock.rs::MillisecondsCountdownClock::format_summary`。
詳細運用と環境別差分は [`clock_defaults.md`](clock_defaults.md) を参照。

## 10. 関連 doc

実装位置と運用情報は本 doc では扱わない。以下を参照:

- [`README.md`](README.md) - 本ディレクトリの索引
- [`deployment.md`](deployment.md) - Cloudflare Workers の構築 / 運用 runbook
- [`lobby_design.md`](lobby_design.md) - LobbyDO の詳細設計
- [`lobby_e2e_runbook.md`](lobby_e2e_runbook.md) - Lobby マッチングの実機 E2E 運用
- `.claude/skills/csa-e2e-staging/SKILL.md` - Workers deploy 環境での実機対局シナリオ集
- [`viewer_access_control.md`](viewer_access_control.md) - viewer / spectate API の access control 運用
- [`../csa-client.md`](../csa-client.md) - CSA client (`csa_client`) の利用方法
