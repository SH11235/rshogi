# Workers LobbyDO + マッチング設計書

本リポ `rshogi-csa-client` を立ち上げっぱなしで複数局を別 AI と自動マッチング
対局させる体験を提供するための設計書。Cloudflare Workers に LobbyDO を新設し、
既存 `GameRoom` DO と組み合わせて Floodgate 相当のマッチングロビーを構築する。

## 1. 背景と目的

現状 (Origin 緩和 / `--target` プリセット / `RECONNECT_GRACE_SECONDS` 有効化 /
auto-reconnect 機能の合流後):

- `rshogi-csa-server-workers` は **1 DO instance = 1 対局** という設計で、`/ws/<room_id>`
  に黒白 2 client が同時接続することで対局成立。
- `rshogi-csa-client` は `--target {staging,production} --room-id <id> --handle <name> --color <black|white>`
  でクイック接続できるが、**room_id を黒白で人手合わせする** 運用が必要。
- 本家 Floodgate (TCP) は 1 server で待機プレイヤをキューに入れ、`game_name`
  が一致した瞬間にペアリング → game_id 発番 → Game_Summary 配布で自動成立する。

これを Workers でも提供したい。設計のキーは **本家 Floodgate のマッチング処理を
LobbyDO 1 個に閉じ込め、ペアリング成立時に既存 GameRoom DO へ handoff** する形。

## 2. アーキテクチャ概要

```
            ┌──────────────────────┐
 csa_client │    /ws/lobby         │   ← LobbyDO (1 個固定 ID = "default")
   ────────▶│   (LOGIN_LOBBY 待機) │
            └──────────────────────┘
                      │
              MATCHED <room_id> <color>
                      ▼
            ┌──────────────────────┐
 csa_client │    /ws/<room_id>     │   ← 既存 GameRoom DO (1 instance = 1 対局)
   ────────▶│  (Game_Summary 配布) │
            └──────────────────────┘
                      │
                 終局 / 切断 / reconnect 不可
                      │
                      └──── csa_client は再度 /ws/lobby に LOGIN_LOBBY して
                            queue に戻る → 次のマッチング待ち
```

- **LobbyDO** (新設、本 PR で scaffold のみ): プレイヤ待機キュー + ペアリング判定 +
  GameRoom DO への引き渡し通知を担当。1 instance 固定 (id_from_name("default"))。
  queue 自体は **メモリのみ** で永続化しない。Hibernation 復帰時は client が
  再 LOGIN_LOBBY して queue を再構成する想定 (queue は揮発でよい)。
- **GameRoom DO** (既存、無変更): 1 対局を駆動。LobbyDO から流入してくる
  client は通常の LOGIN フローでこの DO に接続する。
- **csa_client** (`--lobby` モード追加): ロビーで queue 待機 → MATCHED 通知で
  指定された GameRoom DO に再接続 → 1 局完走 → 再度ロビー queue に戻る、を
  shutdown まで繰り返す。

## 3. プロトコル設計

### 3.1 `/ws/lobby` route

LobbyDO への接続経路。Origin 検査は既存 `forward_ws_to_room` と同じ allowlist を
利用 (Origin 欠落 = 素通し / Origin 付き = allowlist 完全一致)。

### 3.2 LobbyDO 受信プロトコル (client → LobbyDO)

| line | 役割 |
|---|---|
| `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` | queue へエントリ。`<game_name>` がマッチング対象タグ、`<color>` は手番希望 (`black`/`white`/`any`)。 |
| `LOGOUT_LOBBY` | queue から離脱。 |
| `LOBBY_PONG` | LobbyDO が送る `LOBBY_PING` への応答 (双方向 keep-alive)。 |

**`<game_name>` の文字種制限**: `[A-Za-z0-9_-]` のみ許可、長さ 1〜32 文字。これは
`MATCHED <room_id> <color>` の room_id 文字列に `<game_name>` を組み込むため、
区切り曖昧性を排除する目的。形式違反は §3.3 の `LOGIN_LOBBY:incorrect <reason>`
テンプレートで reject (具体的な `<reason>` トークンは `bad_game_name` /
`bad_color` / `bad_id_format` / `bad_format` / `not_login_command` の 5 種、
実装側 `LoginLobbyError::reason` を参照)。

**`<password>` の扱い**: LobbyDO は受信するが値を検証しない (本家 Floodgate と同じ
self-claim 運用、§5.1 参照)。空文字列は `bad_format` で reject されるため、client は
任意の非空 placeholder を送る (csa_client は `--password` 未指定時に `"anything"` を
送る)。

`game_name` の意味: 本家 Floodgate と同じく「同 `game_name` 同士でしかマッチング
しない」。`game_name` をユーザ任意の自由 string にすることで、複数のマッチング
pool を 1 LobbyDO 内で同時稼働できる。

### 3.3 LobbyDO 送信プロトコル (LobbyDO → client)

| line | 役割 |
|---|---|
| `LOGIN_LOBBY:<handle> OK` | queue 登録成功 (`<handle>` は LOGIN_LOBBY で送られた handle 部分をそのまま echo)。 |
| `LOGIN_LOBBY:incorrect <reason>` | 登録失敗 (重複ハンドル / フォーマット不正等)。 |
| `LOGIN_LOBBY:expired` | queue 滞留時間 (TTL) を超過した。client は再度 `LOGIN_LOBBY` を送るか shutdown する。 |
| `MATCHED <room_id> <color>` | ペアリング成立。`<room_id>` と `<color>` は **半角スペース区切り**。client は LobbyDO を close してから `/ws/<room_id>` に `<handle>+<game_name>+<color>` で LOGIN し、対局を開始する。 |
| `LOBBY_PING` | DO Hibernation 復帰時の生存確認。client は `LOBBY_PONG` を返す。 |

`<room_id>` のフォーマット: `lobby-<game_name>-<128bit-rand-hex>` (rand 部分は 32 文字 hex
= 128 bit、衝突確率は事実上ゼロ)。`<game_name>` は §3.2 の文字種制限により
`-` 以外の特殊文字が混入しないため、`-` で区切っても曖昧性なし。

### 3.4 マッチング戦略

ペアリング戦略は **直接マッチのみ** (本設計の最低限ゴール)。同 `game_name` で
`preferred_color` が競合しない先着 2 名を順次ペアリングする。

実装は既存 `rshogi-csa-server::matching::DirectMatchStrategy` を Workers crate
から再利用したいが、**`rshogi-csa-server` crate の wasm32 互換性確認** が前提
となる (実装ロードマップ §6 の (0) 段で実施)。

レート差最小ペアリング等の戦略拡張は本設計のスコープ外 (将来必要になったら
別設計を起こす)。

### 3.5 GameRoom DO への引き渡し

ペアリング成立時、LobbyDO は:

1. `<room_id> = "lobby-<game_name>-<128bit-rand-hex>"` を発番
   (DO storage に既存 room_id 集合を持たない最初の発番でも、128 bit rand により
   GameRoom DO が `id_from_name(<room_id>)` で同名 DO に当たる確率は実質ゼロ)。
2. 黒/白 client それぞれに `MATCHED <room_id> <color>` を送信。
3. LobbyDO 自体の WS は close し、client が `/ws/<room_id>` に再接続するのを
   GameRoom DO 側の `GAME_ROOM_LOGIN_DEADLINE_SECONDS` (§5.3) 内に完了することを
   期待する。LobbyDO は handoff 後の状態を関知しない。
4. GameRoom DO は通常の LOGIN フローでこの 2 client を受け入れて Game_Summary
   配布 → 対局開始。

#### handoff 片側失敗時の整合性

両 client が `/ws/<room_id>` に LOGIN するまでに片側が失敗 (例: 黒は接続成功、
白は失敗) するシナリオがある。これを処理するため:

- **GameRoom DO 側**: `GAME_ROOM_LOGIN_DEADLINE_SECONDS` (既定 60 秒) 内に
  両者 LOGIN が揃わない場合、対局を破棄し DO を空状態に戻す。LOGIN 済み単側
  client には `#CHUDAN handoff_timeout` を送って close。これは GameRoom DO 側の
  既存「対局成立前の片側不在」処理を拡張する形 (実装は本設計のスコープ外、
  実装ロードマップ §6 の (2) 段でカバー)。
- **LobbyDO 側**: handoff 後は GameRoom の状態を関知しない (片側不在の検出は
  GameRoom DO 側が行う)。失敗で戻された client は LobbyDO に再 LOGIN_LOBBY して
  queue に戻る運用 (csa_client が状態機械で実装、§4.1)。

## 4. csa_client `--lobby` モード

```bash
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --lobby \
  --game-name "rshogi-eval" \
  --handle alice \
  --color any \
  --simple-engine \
  --engine /path/to/your/rshogi-usi
```

`--lobby` 指定時は `--room-id` 不要 (LobbyDO が発番)。`--color any` は
`preferred_color = None` で送信される。

### 4.1 状態機械

```
 ┌─────────┐
 │  Init   │
 └─────────┘
      │ ロビー接続
      ▼
 ┌─────────┐
 │ Queued  │← LOGIN_LOBBY:OK 受信
 └─────────┘
      │ MATCHED <room_id> <color> 受信
      ▼
 ┌──────────┐
 │Connecting│ /ws/<room_id> へ LOGIN
 └──────────┘
   │     │
   │     │ LOGIN 失敗 / GameRoom timeout (#CHUDAN handoff_timeout)
   │     └──→ Init に戻り、次ループで再 LOGIN_LOBBY (= Queued へ遷移)
   │
   │ LOGIN OK
   ▼
 ┌─────────┐
 │ Playing │ 通常の対局ループ (auto-reconnect 機能も適用される)
 └─────────┘
   │     │
   │     │ 対局中切断 → auto-reconnect 失敗 (Reconnect_Token expired 等)
   │     └──→ Init に戻り、次ループで再 LOGIN_LOBBY (= Queued へ遷移)
   │
   │ 終局 / shutdown
   ▼
 ┌─────────┐
 │  Init   │ → 次ループで Queued (shutdown なら終了)
 └─────────┘
```

「`Init` に戻る」と「`Queued` (再 LOGIN_LOBBY) に戻る」は同じ遷移を指す:
client 側状態は一旦 `Init` に戻し、main loop の次イテレーションで `acquire_lobby_match`
が走って LOGIN_LOBBY → `Queued` へ遷移する 2 段経路。「Queued に戻る」と書く場合は
この 2 段経路を 1 ステップに圧縮した略記。

`Queued` 状態で `LOGIN_LOBBY:expired` を受けた場合は `Init` に戻り、上記同様 main loop
の次イテレーションで再 LOGIN_LOBBY する (TTL 超過は LobbyDO 側のリソース防衛で発生)。

`shutdown` (Ctrl+C) で `Queued` / `Connecting` / `Playing` から離脱して終了。

**`Connecting` 失敗時のバックオフ戦略**: 既存 `retry_delay` (csa_client main loop) を
そのまま流用する。`config.retry.initial_delay_sec` (既定 10s) から始まり、失敗の度に
2 倍して `config.retry.max_delay_sec` (既定 900s) で頭打ち。最大試行回数は無制限で、
shutdown 信号でのみ離脱する。lobby 専用のバックオフ調整は行わない。

### 4.2 lobby と auto-reconnect (Reconnect_Token 機能) の関係

- `Playing` 状態中に WS 切断 → 既存 auto-reconnect (Reconnect_Token) が走る。
  これは GameRoom DO 内で完結し、LobbyDO は関与しない。
- `Connecting` 状態の失敗 (LobbyDO のペアリング後に GameRoom 接続が失敗) は
  lobby レベルで「マッチング無効」とみなして Queued (再 LOGIN_LOBBY) に戻る。
- `Playing` 中の auto-reconnect が **最終的に失敗** (grace 超過や reconnect token
  reject) した場合も `Init` に戻り、ロビーで次のマッチングを待つ。

## 5. セキュリティと運用

### 5.1 認証

LobbyDO の LOGIN_LOBBY 受け入れ条件:

- `<handle>+<game_name>+<color>` のフォーマットが正しい (既存 GameRoom と同じ)。
- `<game_name>` は §3.2 の文字種制限を満たす。
- `<password>` は Workers 設定 secret と照合する経路は持たない (本家 Floodgate と
  同じく「ハンドル名は self-claim」運用)。
- 重複ハンドルは別セッションを EvictOld (既存 League 実装と同じ挙動)。

### 5.2 Origin 検査

`/ws/lobby` も `/ws/<room_id>` と同じ Origin 検査を通す
(`router.rs::forward_ws_to_lobby` を新設、`evaluate(origin, allow_list)` を流用)。

### 5.3 リソース上限

各 config 値は `[vars]` の追加キーとして wrangler.toml.example に追加し、CI 整合
チェック (`tests/wrangler_template_consistency.rs`) で固定する:

| Config キー | 既定値 | 実装場所 | 役割 |
|---|---|---|---|
| `LOBBY_QUEUE_SIZE_LIMIT` | `100` | LobbyDO | 1 LobbyDO 内の queue 総数上限 (`game_name` 別ではなく合計)。超過時 LOGIN_LOBBY を reject。 |
| `LOBBY_QUEUE_TTL_SECONDS` | `300` | LobbyDO | LOGIN_LOBBY からのエントリ有効期限。expire 時 `LOGIN_LOBBY:expired` で client を起こす。 |
| `GAME_ROOM_LOGIN_DEADLINE_SECONDS` | `60` | GameRoom DO | `MATCHED` 受信後、両 client が GameRoom DO に LOGIN するまでの猶予。経過しても両者揃わなければ `#CHUDAN handoff_timeout` で broadcast → DO 破棄 (§3.5)。 |

> 設計初期は `LOBBY_HANDOFF_DEADLINE_SECONDS` (LobbyDO 側の名前) と
> `GAME_ROOM_LOGIN_DEADLINE_SECONDS` (GameRoom DO 側の名前) を別キーとして
> 議論したが、handoff 完了の判断主体は GameRoom DO のみ (LobbyDO は handoff 後の
> client 状態を関知しない、§3.5)。同じ意味を 2 つのキーで表現すると運用設定で値が
> ずれて挙動が分裂しうるため、**`GAME_ROOM_LOGIN_DEADLINE_SECONDS` 1 本に統一**
> する。LOBBY_ prefix のキーは追加しない。

### 5.4 metrics / observability

- `wrangler tail` ログに `[Lobby] queue=<n> matched=<m>` を 30 秒ごとに出力。
  Hibernation 中は Worker code が走らないため、`state.alarm()` API で 30 秒後の
  alarm を仕掛けて起床する経路にする (LobbyDO は queue 保持中は idle になりにくいが、
  queue 空時は Hibernation に入りうるので保険)。
- DO storage に `total_matches_made` を増分 (運用観測用、実装ロードマップ §6 (5) 段)。

## 6. 実装ロードマップ (本設計後の follow-up PR)

各段は独立 PR として merge 可能で、依存関係は (0) → (1) → (2) → (3) → (4) → (5)
の直線的な順序を想定する。

| 段 | 概要 | スコープ |
|---|---|---|
| (0) wasm32 互換確認 | `rshogi-csa-server::matching` の wasm32 ビルド確認 | `cargo check -p rshogi-csa-server --no-default-features --features workers --target wasm32-unknown-unknown` で **互換確認済み** (workers feature で tokio が外れる)。非互換だった場合に備えていた no_std 互換 sub-crate 切り出しの fallback 経路は本実装では不要 |
| (1) LobbyDO 骨格 + `/ws/lobby` route | 空 DO + LOGIN_LOBBY 受信のみ | in-memory queue、ペアリング無し、`<game_name>` 文字種検証 |
| (2) DirectMatch ペアリング + handoff | 既存 `DirectMatchStrategy` を Workers crate に wrap、`MATCHED` 送信、GameRoom DO 側の login deadline | queue が 2 件揃ったらペアを発番、`MATCHED` 送信。GameRoom DO 側に `GAME_ROOM_LOGIN_DEADLINE_SECONDS` 経過で破棄経路を追加 |
| (3) csa_client `--lobby` mode | 状態機械 (Init / Queued / Connecting / Playing) を実装 | `--lobby` フラグ、shutdown 経路、再 LOGIN、auto-reconnect 機能と整合 |
| (4) Miniflare smoke | 2 client 同時接続 → 1 ペア成立 → handoff → 1 局完走 | CI E2E。auto-reconnect 機能後の GameRoom 新規 LOGIN パスが壊れていないことの回帰確認も含む |
| (5) metrics / Hibernation 対応 + queue TTL | 30 秒 keep-alive、queue TTL、storage カウンタ | 信頼性向上 |

(0)〜(4) まで揃えばゴール「本リポ csa_client が立ち上げっぱなしで複数局を
マッチング対局する」が最低限達成できる。(5) は queue 滞留 / 観測性の本番運用前
強化 PR。queue TTL に依存する `LOGIN_LOBBY:expired` は (5) で初めて実装可能。

## 7. 既存設計との比較

| 観点 | 本家 Floodgate (TCP) | rshogi Workers (LobbyDO 設計後) |
|---|---|---|
| プロセス境界 | 1 プロセス内に League + 全対局 | LobbyDO + N 個の GameRoom DO |
| マッチング | League::confirm_match で内部即時 | LobbyDO で発番 → MATCHED 通知 → client が GameRoom に再接続 |
| 対局並行度 | 1 server = N 対局 | 1 GameRoom DO = 1 対局 (Cloudflare account-level の concurrent instance 数上限内で実用上ほぼ制約なし) |
| 切断後再接続 | 同 server に reconnect | 元 GameRoom DO に reconnect (Reconnect_Token 機能) |
| 休止 | 常時実行 | GameRoom DO は Hibernation で対局 idle 時 0 課金 (LobbyDO は queue 保持中は idle になりにくく、空時のみ Hibernation 対象) |

設計上の妥協点: client は **マッチング成立後に新規 WS 接続を 1 回張り直す** 必要がある。
本家 Floodgate は同 TCP 接続上で対局が始まるので 1 hop だが、Workers の DO 境界を
跨ぐため 2 hop (Lobby → GameRoom) になる。レイテンシ観点では数百 ms 増だが、
1 game の総対局時間に対して無視できる。
