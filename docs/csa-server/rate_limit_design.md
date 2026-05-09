# Rate limit / abuse protection 設計メモ ([#622](https://github.com/SH11235/rshogi/issues/622))

Floodgate audit (2026-05-08) で起票された P0 [#622](https://github.com/SH11235/rshogi/issues/622)
の実装着手前 design doc。既存の audit 結論と Codex 相談 (2026-05-09) の決定事項
を反映し、実装フェーズに入る前に user 確認すべき open question を集約する。

本 doc は **設計合意の前段** として draft で merge する。実装 PR を切る時に
本 doc も最終版に書き換える契約。

## 1. スコープ

| 含む | 含まない (別 issue / 別 PR) |
|---|---|
| `/ws/<room_id>` / `/ws/lobby` の WS upgrade 直前の rate limit | viewer 配信 API (`/api/v1/games*`) の rate limit (将来 [#560](https://github.com/SH11235/rshogi/issues/560) 系で扱う) |
| LOGIN_LOBBY / CHALLENGE_LOBBY の 1 IP / 1 handle あたり頻度制限 | LOGIN handle 自称強化 (`#621` follow-up [#664](https://github.com/SH11235/rshogi/issues/664) 側のスコープ) |
| GameRoom DO の連続生成上限 (1 IP あたり 1 分の room 作成数) | DO 内 admin command の rate limit (#663 で session 制限済) |
| Cloudflare WAF / Rate Limiting Rules の運用手順 (runbook) | DDoS 攻撃検知の SIEM 連携 (Session B [#625](https://github.com/SH11235/rshogi/issues/625) 側) |

## 2. 攻撃モデル / 緩和ターゲット

### 2.1 想定攻撃ケース

| 攻撃 | 対象 | 影響 | 緩和層 |
|---|---|---|---|
| 大量 room_id への WS upgrade flood | `/ws/<room_id>`、GameRoom DO | DO instance 量産 → memory / storage / class A request 浪費 | 層 1 (WAF) + 層 2 (atomic rate limiter) + 層 3 (room 作成 counter) |
| LOGIN_LOBBY flood | `/ws/lobby`、LobbyDO | LobbyDO WS 上限 32,768 接続/DO → 全マッチング停止 | 層 1 (WAF) + 層 2 (atomic rate limiter) |
| AGREE 不到達による DO 占有 | GameRoom DO | AGREE_TIMEOUT_SECONDS 経過まで slot 占有 (現状 60 sec、PR [#616](https://github.com/SH11235/rshogi/pull/616) で対処済) + その間に新規流入で複数 DO 占有 | 既存 AGREE_TIMEOUT_SECONDS + 本 issue の per-IP cap |
| handle 自称による queue 浪費 | LobbyDO in-memory queue | LOBBY_QUEUE_SIZE_LIMIT (= 100 既定) を埋めて正規ユーザを締め出す | 層 2 (per-handle atomic counter) + #664 (handle whitelist) |

### 2.2 緩和ターゲット (Codex review 反映 2026-05-09)

3 層構成、層 2 は **atomic な counter 機構** が baseline、WAF = production 防御層 として併用前提。

- **層 1: Cloudflare WAF / Rate Limiting Rules** (production 防御)
  - `/ws/lobby` / `/ws/<room_id>` への 1 IP あたり N 接続/分の上限
  - Cloudflare ダッシュボード or API で設定 (運用層、コード変更不要)
  - production 既設の Cloudflare Free plan で WAF Rate Limiting が利用可能か要確認 (Free plan は基本制限機能のみ、Pro 以上で柔軟な rule 定義)
- **層 2: Worker code 内 atomic rate limiter** (baseline)
  - `accept_web_socket` 直前に 1 IP / 1 handle ごとの bucket / counter を参照、超過すれば 429 / 503 で拒否
  - **atomic 操作が必須** (Cloudflare KV は eventual consistency のため race で抜ける、Codex review P1)。
    Cloudflare Workers Rate Limiting binding か専用 DO を baseline、KV は metrics / 観測補助のみ — open question §3
- **層 3: room 作成上限**
  - 1 IP あたり 1 分間の **新規 GameRoom DO 起動回数** に上限
  - LobbyDO `MATCHED` 経由 (#582 `CHALLENGE_LOBBY` も含む) と `/ws/<room_id>` 直行の両経路で発火
  - 状態保管は層 2 と同じ atomic counter (KV は不可)
  - 発火ポイント (claude review) — 詳細は §4.4 Implementation hook points 参照

### 2.3 共通実装ガード (Cloudflare Workers 固有)

- **クライアント IP の取得元**: `request.headers().get("CF-Connecting-IP")` のみを使う。
  `X-Forwarded-For` は Cloudflare 通過後も client が偽装可能なヘッダを含むため、本 PR の rate limit 鍵には使わない (claude review)
- **`CF-Connecting-IP` 未取得時の挙動**: **fail-closed (拒否)**。Cloudflare Workers
  では正規経路で常に存在するため、欠落は anomalous (内部 fetch / mock / 攻撃いずれも
  通常運用では出ない)。本 PR では `CF-Connecting-IP` が空 / 取得失敗の request を
  503 + `Retry-After` 短時間 (10 秒程度) で reject する設計に固定する (§5.3 で
  fail-closed テストを必須化)

## 3. Open questions (user 確認必須)

実装着手前に user に確認したい設計判断を 5 件集約する。本 doc 更新 PR の review
段階で確定 → 実装 PR で値を埋める契約。

### Q1. Cloudflare WAF API access の有無

- 質問: production 運用 Cloudflare アカウントで WAF / Rate Limiting Rules の **API token + zone access** が
  自動化 (CI / Terraform) で扱える状態か?
- 選択肢:
  - **Q1-A**: 現時点で API token 整備済 → 実装 PR で WAF rule を IaC (Cloudflare Terraform Provider 等) で
    管理し、`docs/csa-server/rate_limit.md` に rule 定義を載せる
  - **Q1-B**: API token 未整備 → 本 PR では runbook (`docs/csa-server/rate_limit.md`) でダッシュボード手順
    のみ提示し、API/IaC 管理は別 follow-up issue
  - **Q1-C**: Cloudflare Free plan で Rate Limiting Rules が制限的 → 層 1 を **Worker code 側で代替** (層 2 に
    集約)、production 昇格時に Pro 以上へ移行を検討
- recommendation: **Q1-B (runbook only)** をまず取って実装 PR を軽くし、Cloudflare 側の plan / API token
  の整備が固まってから IaC 化を別 follow-up で扱う

### Q2. 層 2 の atomic rate limiter 実装 (Codex review P1 反映)

- 質問: 1 IP / 1 handle ごとの counter / bucket をどの atomic 機構で実装するか?
- **前提 (Codex P1 確定)**: Cloudflare KV は eventual consistency で
  `read → modify → write` の atomic op 保証がなく、同一 IP の同時 burst で
  race して上書きが起きる ([Cloudflare 公式: How KV works](https://developers.cloudflare.com/kv/concepts/how-kv-works/))。
  したがって **KV は層 2 の baseline には使わない**。
- 選択肢:
  - **Q2-A: Cloudflare Workers Rate Limiting binding** (Workers 標準 API、atomic)
    - メリット: native atomic、運用コスト極小、設定は wrangler binding のみ。
      [公式 docs](https://developers.cloudflare.com/workers/runtime-apis/bindings/rate-limit/) 参照
    - デメリット: Cloudflare 提供アルゴリズム (sliding window) しか選べない、custom
      semantics (例: token bucket の refill rate を IP/handle で変える) は不可
  - **Q2-B: 専用 RateLimitDO (singleton or sharded)**
    - メリット: strong consistency、token bucket の atomic 操作、custom logic
      (handle prefix / multi-window 等) 自由
    - デメリット: 1 instance だと SPOF / scaling 限界 ([#632](https://github.com/SH11235/rshogi/issues/632)
      LobbyDO sharding と同テーマ)。sharding 必須なら追加実装コスト大
  - **Q2-C: GameRoom / Lobby DO 内 in-memory** (既存 DO 局所 bucket、補助のみ)
    - メリット: 配線最小、追加 DO 不要
    - デメリット: per-IP 集約には DO 横断ができないので、room ごとの burst しか
      防げない。LOBBY 経路 (1 instance) では機能するが GameRoom (per-room instance)
      では集約不能
  - **Q2-D: KV (補助のみ、baseline 不可)**
    - 用途: 観測 metrics / soft cap (1 分単位 best-effort) としてのみ使用、
      hard rate limit には使わない
    - デメリット: race による counter 抜けあり、強制 fail-closed が困難
- recommendation: **Q2-A (Workers Rate Limiting binding) を baseline**。custom
  semantics が必要になった時点で Q2-B (専用 DO) に拡張。Q2-C は LobbyDO 内の
  per-handle prefix 集約用に併用検討、Q2-D は metrics 観測 only
- 採択判定材料:
  - production Cloudflare 契約で Rate Limiting binding が利用可能か (要 user 確認)
  - 利用不可なら Q2-B (専用 DO) に降格、初期は singleton で実装し負荷測定後に sharding 検討

### Q3. Rate limit の閾値 (各層、初期値)

- 質問: production 既定値をいくつにするか?
- 選択肢のテーブル (Codex P2 反映: `CHALLENGE_LOBBY` も対象に追加):
  | 層 | 単位 | 提案値 (recommendation) | 緩和したい場合 |
  |---|---|---|---|
  | 層 1 (WAF) | 1 IP あたり `/ws/lobby` 接続 | 30 接続/分 | (Pro 以上に依存) |
  | 層 1 (WAF) | 1 IP あたり `/ws/<room_id>` 接続 | 60 接続/分 | (同上) |
  | 層 2 (atomic limiter) | 1 IP / LOGIN_LOBBY | 10 trial/分 | env で `LOBBY_LOGIN_RATE_PER_IP_PER_MIN` |
  | 層 2 (atomic limiter) | 1 handle / LOGIN_LOBBY | 5 trial/分 | env で `LOBBY_LOGIN_RATE_PER_HANDLE_PER_MIN` |
  | 層 2 (atomic limiter) | 1 IP / CHALLENGE_LOBBY (private 招待発行) | 5 trial/分 | env で `LOBBY_CHALLENGE_RATE_PER_IP_PER_MIN` |
  | 層 2 (atomic limiter) | 1 inviter handle / CHALLENGE_LOBBY | 3 trial/分 | env で `LOBBY_CHALLENGE_RATE_PER_HANDLE_PER_MIN` |
  | 層 3 (atomic counter) | 1 IP / GameRoom 起動 | 20 room/分 | env で `ROOM_CREATE_RATE_PER_IP_PER_MIN` |
- `CHALLENGE_LOBBY` の閾値根拠: 1 inviter が短時間に多数の private 招待を発行
  する正規ユースケースは想定しないため、LOGIN_LOBBY より低めに設定。token registry
  ([`lobby.rs::handle_challenge_lobby`](../../crates/rshogi-csa-server-workers/src/lobby.rs)) を
  flood で埋めることを防ぐ
- recommendation: 初期値は保守的 (推奨表) で staging E2E + 実トラフィック観測後に調整
- env 経由で動的調整可能にする (config.rs 経由、`LOCAL_DEV_ONLY_VARS_KEYS` 範囲外で
  `SHARED_PUBLIC_VARS_KEYS` 入り)

### Q4. 拒否時のレスポンス / UX

- 質問: 上限超過時にクライアントに何を返すか?
- 選択肢:
  - **Q4-A**: WS upgrade を 503 で拒否 + `Retry-After` ヘッダ (LOGIN_LOBBY なら `LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>`)
  - **Q4-B**: 静かに切断 (1009 Message Too Big 経路と同じ pattern)
  - **Q4-C**: 専用 close code (例: 4000-4999 範囲のカスタムコード)
- recommendation: **Q4-A** + 既存 `LOGIN_LOBBY:incorrect rate_limited` パターン (memory 由来、`server.rs::handle_connection`
  で実装済) を再利用。HTTP path は 429 で `Retry-After` を返す
- 注意: Q4-A 採択時、**Q5 で Floodgate client の `LOGIN_LOBBY:incorrect rate_limited` 解釈確認** が pre-PR3a
  blocker になる

### Q5. Floodgate / 既存 client の `rate_limited` 互換性 (claude review 追加)

- 質問: `LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>` を返したとき、既存
  client が wire format をパースして reasonable な UX (例: 自動 retry を
  抑える / エラーメッセージ表示) を取れるか?
- 選択肢:
  - **Q5-A**: pre-PR3a で `crates/rshogi-csa-client` (本リポ同梱) と
    `crates/rshogi-csa-server-tcp` の `LOGIN_LOBBY:incorrect` パーサ実装を grep して
    `rate_limited` トークンが既知 reason 一覧 (`unknown_game_name` /
    `already_logged_in` 等) に追加されているか / 未知トークンの fallback 経路が
    安全か (= 自動 retry loop を発生させないか) を確認する。Floodgate 互換 native
    client (本リポ外) については protocol-reference.md 経由でのみ告知できる
  - **Q5-B**: pre-PR3a checklist を省略し、PR3a 実装と同時に
    `lobby_protocol.rs::build_login_incorrect_line` の format を確認・整合
- recommendation: **Q5-A**。実装着手前に 30 分程度で grep + 対象 client の手動
  シナリオ確認を済ませ、互換性問題があれば閾値レスポンスを `Q4-B` (silent close) や
  `Q4-C` (専用 close code) に変える設計分岐ポイントとする

## 4. 実装計画 (Q1-Q5 確定後の素案)

### 4.1 PR 構成案

1 PR にすると差分が大きいので 2 PR に分割提案。**`rate_limit.rs` の中身は Q2 採択
結果で大きく変わる**ため、Q2 確定前に着手しない:

- **PR3a: 層 2 + 層 3 + runbook** (Worker code-only)
  - `crates/rshogi-csa-server-workers/src/rate_limit.rs` (新規 module): Q2 採択次第:
    - **Q2-A 採択時**: Cloudflare Workers Rate Limiting binding (`env.RATE_LIMITER`) の
      薄い adapter のみ。`check_login_lobby(env, ip)` 等の呼び出し helper と env→threshold
      マッピング、`Retry-After` 値計算が中心。token consume / refill は Cloudflare 側
      実装に委譲し、本 crate には pure な token bucket logic は書かない
    - **Q2-B 採択時 (Q2-A 不可の fallback)**: 専用 RateLimitDO の class 実装 +
      薄い adapter。pure な token bucket logic (consume / refill / window 切替) は
      DO 内 storage を読み書きする atomic 操作として書く必要があり、host テストで
      pure helper として切り出す
  - `router.rs` / `lobby.rs` / `game_room.rs` の WS upgrade 経路で `rate_limit::check_*` 呼び出し
  - `ConfigKeys` に閾値 env 追加 (Q3 表参照)
  - `docs/csa-server/rate_limit.md` (runbook、WAF ダッシュボード手順 + 環境変数 reference)
- **PR3b: WAF / IaC** (Cloudflare 側、Q1-A 採択時のみ)
  - Terraform / wrangler API 経由の WAF rule 定義
  - production / staging の rule 差分 doc

Q1-B / Q1-C 採択時は PR3b は doc 更新のみ。

### 4.2 既存 audit 領域との統合

- [#627](https://github.com/SH11235/rshogi/issues/627) で導入した `MAX_WS_LINE_BYTES` (4 KiB) は
  per-message DoS 防御。本 PR とは別軸で併存
- [#600](https://github.com/SH11235/rshogi/issues/600) `AGREE_TIMEOUT_SECONDS` は対局成立前 DO 占有
  の自動解放。本 PR の per-IP cap と独立に動く
- [#629](https://github.com/SH11235/rshogi/issues/629) orphan sweep は cron 駆動の DO 後始末。
  本 PR で room 作成 cap を入れることで sweep 対象の orphan 量も減らせる

### 4.3 LobbyDO sharding ([#632](https://github.com/SH11235/rshogi/issues/632)) との関係

Q2-B (専用 RateLimitDO) を採用すると DO 1 instance の SPOF / scaling 限界が
[#632](https://github.com/SH11235/rshogi/issues/632) と同じテーマになる。本 PR で
RateLimitDO を新設する場合、Session C [#632](https://github.com/SH11235/rshogi/issues/632)
の sharding 戦略と整合する設計を一度で固める方が手戻り少。

→ Q2-A (Cloudflare Rate Limiting binding) 採択ならこの懸念は浮かない (Cloudflare 側で sharding 済)。

### 4.4 Implementation hook points (claude review #1 反映)

層 3 の room 作成カウンタ発火点は **DO 起動経路 2 つ** で挙動が分かれるため、
PR3a 実装前に確定しておく。両経路で **DO の `id_from_name` を解決した直後 +
当該 DO への最初の fetch / WS upgrade を発出する直前** にカウンタを増やす設計。

| 経路 | 発火ファイル / 関数 | カウンタ更新タイミング |
|---|---|---|
| `/ws/<room_id>` 直行 | [`router.rs::forward_ws_to_room`](../../crates/rshogi-csa-server-workers/src/router.rs) | Origin 検査 → `parse_ws_route` 直後、`stub.fetch_with_request` 直前 |
| LobbyDO `MATCHED` 経由 | [`lobby.rs::handle_login_lobby` の `try_pair` 後](../../crates/rshogi-csa-server-workers/src/lobby.rs)、`build_matched_line` 送出経路 | 両者 client に `MATCHED <room_id>` を送る前 (送出後だと client 側再 LOGIN 経路で 2 重カウントになるため) |
| CHALLENGE_LOBBY private 経由 (#582) | LobbyDO で private LOGIN ペアが揃った瞬間 (上記同等の MATCHED 経路、follow-up issue #582 の実装時に確認) | 同上 |

注意点 (claude review):
- DO 存在確認前にカウンタを増やすと、`%%room_id` が空 / 不正な値の場合でも
  カウントされる。`parse_ws_route` でフォーマット検証 → 通過 → カウント、の順序を守る
- カウンタ自体の更新は層 2 と同じ atomic 機構 (Q2 採択結果に従う)。KV を使った
  best-effort 設計は採用しない (Codex P1)

### 4.5 KV を補助用途に限定した場合の運用 (Q2-D 採択時のみ)

KV は metrics 観測 (1 分単位 best-effort、aggregate ダッシュボード等) や
soft cap warning にのみ使う。以下を明示する:

- **Window kind**: タンブリングウィンドウ (`key = ip:202605081234` の分単位、
  KV TTL=60s と整合)。スライディングは read-modify-write 回数が増えて KV 経路では
  実用的でない
- **Fail mode**: KV が一時的に利用不能なら **fail-open (rate limit 観測のみスキップ)**。
  hard cap は層 2 の atomic 機構が保証するため、KV 障害で全拒否 (fail-closed) する
  必要はない
- **整合性ノイズの許容**: KV の最終整合 (~60s) で counter が 1〜2 件抜けても、
  hard cap は層 2 で保証されるため運用影響は無視可能

## 5. Test plan (実装 PR 用 checklist)

### 5.1 Pre-PR3a checklist

- [x] **Q5 互換性確認** (2026-05-10 確認済): `crates/rshogi-csa-client` と
  `crates/rshogi-csa-server-tcp` で `LOGIN(_LOBBY):incorrect <reason>` パーサ実装を
  grep し、`rate_limited` トークン受信時の挙動を確認した (詳細: §6.1)。
  - csa-client (LOGIN / LOGIN_LOBBY 両経路) は `bail!` で fatal exit、
    自動 retry なし → loop リスクなし
  - 既存 `<reason>` トークンの内容は client 側で評価していない (prefix 判定のみ)
  - TCP server 側に `LOGIN:incorrect rate_limited retry_after=<sec>` precedent あり
    ([`crates/rshogi-csa-server-tcp/src/server.rs::handle_connection`](../../crates/rshogi-csa-server-tcp/src/server.rs)、
    test [`tests/tcp_session.rs::login_rate_limit_denies_burst`](../../crates/rshogi-csa-server-tcp/tests/tcp_session.rs))
  - **Q4-A の `rate_limited retry_after=<sec>` を採択確定**。Q4-B/Q4-C への切替は不要
- [ ] **Q1 確定**: IaC ロードマップ [#675](https://github.com/SH11235/rshogi/issues/675) Phase 3
  との合流で **Q1-A (IaC for WAF)** に確定見込み。production Cloudflare 契約で
  Rate Limiting Rules が利用可能か user 確認
- [ ] **Q2 確定**: Workers Rate Limiting binding が使えなければ Q2-B (専用 DO) に
  降格する判断

### 5.2 Host unit tests (Q2 採択結果に応じた pure logic 範囲)

- [ ] `crates/rshogi-csa-server-workers/src/rate_limit.rs` の pure helper を host テストでカバー
  - **Q2-A 採択時**: env→threshold マッピング解決 / `Retry-After` 秒数算出 /
    Cloudflare binding error の error mapping (atomic op 自体は Cloudflare 側保証)
  - **Q2-B 採択時**: token consume / refill / window 切替 / 多 IP 並列 (DO storage
    に対する atomic 操作の pure logic を切り出してテスト)
- [ ] CHALLENGE_LOBBY ハンドラの per-IP / per-handle counter 増減 logic
- [ ] `cargo test -p rshogi-csa-server-workers --lib rate_limit` 全 pass

### 5.3 Miniflare E2E (`tests/miniflare_smoke/rate_limit.test.ts` 新規)

- [ ] **LOGIN_LOBBY flood** (per-IP): 同一 IP から N+1 回 LOGIN_LOBBY を投げ、
  N+1 回目に `LOGIN_LOBBY:incorrect rate_limited` で拒否
- [ ] **LOGIN_LOBBY flood** (per-handle): 同一 handle で異なる IP から M+1 回
  LOGIN_LOBBY を投げ、M+1 回目に拒否
- [ ] **CHALLENGE_LOBBY flood** (per-IP / per-inviter): 同様の per-IP /
  per-inviter limit 動作 (Codex P2)
- [ ] **/ws/<room_id> upgrade flood**: 同一 IP から M+1 回 `/ws/<room_id>` upgrade
  で 503 + `Retry-After` レスポンス
- [ ] **窓リセット**: 1 分後に counter がリセットされて受理される
- [ ] **CF-Connecting-IP 欠落時の挙動** (claude review #3): `CF-Connecting-IP`
  ヘッダなしの request で fail-closed (拒否) されること
- [ ] **`%%room_id` 不正値**: `parse_ws_route` で reject される room_id では
  カウンタ増加しないこと (claude review #1)

### 5.4 並行 / 障害シナリオ (claude review #5)

- [ ] **並行 burst テスト**: 同一 IP から複数の Worker インスタンスが並行して
  layer 2 counter を increment するとき、atomic 機構 (Q2 結果) で **race による
  counter 抜けが発生しないこと**。Q2-A (Workers Rate Limiting binding) 採択時は
  Cloudflare 側保証だがテストで実証する
- [ ] **KV (Q2-D 採択時のみ): KV 障害時 fail-open テスト**: KV binding が `throw`
  しても layer 2 の hard cap は層 2 atomic 機構で機能し、観測 metrics のみ欠落
  すること

### 5.5 ビルド / 統合

- [ ] `cargo build --target wasm32-unknown-unknown --lib --release` green
- [ ] staging E2E: 大量 LOGIN flood 試験で全マッチング停止が回避されることを観測
- [ ] runbook (`docs/csa-server/rate_limit.md`) で WAF dashboard 手順 + env tuning gradient を doc 化

## 6. 補足調査結果

### 6.1 Q5 client 互換性 grep (2026-05-10 実施)

`Q4-A` (`LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>`) を本リポ内 client 群に
送信したときの挙動を grep で確認した結果。

#### TCP server (`rshogi-csa-server-tcp`)

`LOGIN:incorrect rate_limited retry_after=<sec>` を **既に実装済み**:

- [`src/rate_limit.rs::IpLoginRateLimiter`](../../crates/rshogi-csa-server-tcp/src/rate_limit.rs):
  per-IP 1 分窓カウンタ、超過時 `RateDecision::Deny { retry_after_sec }`
- [`src/server.rs::handle_connection`](../../crates/rshogi-csa-server-tcp/src/server.rs)
  (line 1088-1100 付近): 既定 10 trial/分、超過時に `LOGIN:incorrect rate_limited
  retry_after={retry_after_sec}` 行を送って `return`
- [`tests/tcp_session.rs::login_rate_limit_denies_burst`](../../crates/rshogi-csa-server-tcp/tests/tcp_session.rs):
  12 連続 LOGIN で 11 回目以降に `LOGIN:incorrect rate_limited` が返ることを assert

→ Workers 側で同 format を採用すれば TCP との挙動整合が取れる。precedent が確立済。

#### csa-client (`rshogi-csa-client`)

LOGIN (game room / TCP 経路): [`src/protocol.rs::login`](../../crates/rshogi-csa-client/src/protocol.rs) line 125-141:

```rust
if is_login_ok(&response) {
    log::info!("[CSA] ログイン成功: {id}");
    Ok(())
} else {
    bail!("ログイン失敗: {response}");  // ← 任意の <reason> を含めて fatal exit
}
```

- `is_login_ok` は `LOGIN:` prefix + ` OK` suffix のセット判定 ([line 594])
- `<reason>` トークン (`unknown_game_name` / `already_logged_in` /
  `rate_limited retry_after=10` 等) を区別せず、**全ケースで `bail!`**
- **自動 retry なし** → `rate_limited` 受信で client が retry loop に入るリスクなし

LOGIN_LOBBY (Workers lobby 経路): [`src/main.rs:577-631`](../../crates/rshogi-csa-client/src/main.rs):

```rust
if let Some(rest) = line.strip_prefix("LOGIN_LOBBY:") {
    if rest.ends_with(" OK") { /* MATCHED 待機 */ continue; }
    if rest.starts_with("incorrect") {
        bail!("[Lobby] LOGIN_LOBBY 拒否: {rest}");
    }
    if rest == "expired" { bail!(...); }
}
// 未知 line は log::debug! で無視 (continue)
```

- `LOGIN_LOBBY:incorrect <reason>` は `<reason>` の中身を見ず一律 `bail!`
- 未知 prefix は debug log で無視するが、`incorrect` 系で `<reason>` を増やしても
  fatal exit のまま動く
- **自動 retry なし**

#### 結論

- **Q4-A 採択は安全**。csa-client 側に retry loop 発生のリスクなし、`rate_limited`
  トークンを区別する必要なし
- 外部 Floodgate 互換 native client (本リポ外) は通常 `/ws/<room_id>` (LOGIN) で
  接続するため `LOGIN_LOBBY:incorrect` プレフィックス自体に出会わない。LOGIN 経路で
  `LOGIN:incorrect rate_limited retry_after=<sec>` を返すのは TCP server が既に
  precedent を持っているので互換性問題なし
- 副次改善案 (本 PR 範囲外、follow-up): csa-client 側で `retry_after=<sec>` を
  parse してログを「`X 秒待ってから再試行`」形式で出す。UX 改善のみで security 影響なし

## 7. 関連

- 親 issue: [#622](https://github.com/SH11235/rshogi/issues/622)
- 並走 (Session A 同パッケージ):
  - PR [#662](https://github.com/SH11235/rshogi/pull/662) (#560、admin auth foundation) — merged
  - PR [#663](https://github.com/SH11235/rshogi/pull/663) (#621、admin command) — merged
  - [#664](https://github.com/SH11235/rshogi/issues/664) (#621 follow-up、LOGIN handle 自称強化)
- Session C (capacity) との依存: [#632](https://github.com/SH11235/rshogi/issues/632) (LobbyDO sharding)
  と RateLimitDO 設計の重複に注意
- Session B (ops) との依存: [#625](https://github.com/SH11235/rshogi/issues/625) (alerting / metrics)
  で本 PR の rate limit 拒否カウントを観測する経路を別途整備
