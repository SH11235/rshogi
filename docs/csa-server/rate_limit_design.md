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
| 大量 room_id への WS upgrade flood | `/ws/<room_id>`、GameRoom DO | DO instance 量産 → memory / storage / class A request 浪費 | 層 1 (WAF) + 層 2 (Worker token bucket) + 層 3 (room 作成 counter) |
| LOGIN_LOBBY flood | `/ws/lobby`、LobbyDO | LobbyDO WS 上限 32,768 接続/DO → 全マッチング停止 | 層 1 (WAF) + 層 2 (Worker token bucket) |
| AGREE 不到達による DO 占有 | GameRoom DO | AGREE_TIMEOUT_SECONDS 経過まで slot 占有 (現状 60 sec、PR [#616](https://github.com/SH11235/rshogi/pull/616) で対処済) + その間に新規流入で複数 DO 占有 | 既存 AGREE_TIMEOUT_SECONDS + 本 issue の per-IP cap |
| handle 自称による queue 浪費 | LobbyDO in-memory queue | LOBBY_QUEUE_SIZE_LIMIT (= 100 既定) を埋めて正規ユーザを締め出す | 層 2 (per-handle token bucket) + #664 (handle whitelist) |

### 2.2 緩和ターゲット (合意済 by Codex consultation)

3 層構成、Worker token bucket = baseline、WAF = production 防御層 として併用前提。

- **層 1: Cloudflare WAF / Rate Limiting Rules** (production 防御)
  - `/ws/lobby` / `/ws/<room_id>` への 1 IP あたり N 接続/分の上限
  - Cloudflare ダッシュボード or API で設定 (運用層、コード変更不要)
  - production 既設の Cloudflare Free plan で WAF Rate Limiting が利用可能か要確認 (Free plan は基本制限機能のみ、Pro 以上で柔軟な rule 定義)
- **層 2: Worker code 内 token bucket** (baseline)
  - `accept_web_socket` 直前に 1 IP / 1 handle prefix あたりのバケットを参照、超過すれば 429 / 503 で拒否
  - 状態保管先は KV (グローバル一貫性) or DO counter (region 局所一貫性) のどちらかを選ぶ — open question §3
- **層 3: room 作成上限**
  - 1 IP あたり 1 分間の **新規 GameRoom DO 起動回数** に上限
  - Worker KV counter で per-IP-per-minute window を持つ
  - LobbyDO `MATCHED` 経由 (#582 `CHALLENGE_LOBBY` も含む) と `/ws/<room_id>` 直行の両経路で発火

## 3. Open questions (user 確認必須)

実装着手前に user に確認したい設計判断を 4 件集約する。本 doc 更新 PR の review
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

### Q2. Worker token bucket の状態保管先 (層 2)

- 質問: 1 IP / 1 handle ごとの bucket 残量をどこに保管するか?
- 選択肢:
  - **Q2-A: Cloudflare KV** (eventual consistency, 60 秒 TTL)
    - メリット: 全 region で同一 IP の状態を共有できる、運用コスト低
    - デメリット: `KV.put` は最終整合 (~60 sec の伝播ラグ)、burst 攻撃で window 内の counter が遅延し抜ける
  - **Q2-B: 専用 RateLimitDO (singleton or sharded)**
    - メリット: strong consistency、token bucket の atomic 操作
    - デメリット: 1 instance だと SPOF / scaling 限界 ([#632](https://github.com/SH11235/rshogi/issues/632)
      LobbyDO sharding と同テーマ)。sharding 必須なら追加実装コスト大
  - **Q2-C: GameRoom / Lobby DO 内 in-memory** (既存 DO に局所 bucket)
    - メリット: 配線最小、追加 DO 不要
    - デメリット: per-IP の集約には DO 横断ができないので、room ごとの connection burst しか防げない
- recommendation: **Q2-A (KV) を baseline、Q2-B を後続検討**。KV の最終整合は層 1 (WAF) で
  カバーし、層 2 は best-effort で OK
- 影響: 採用次第で本 PR 実装範囲が大きく変わる (KV binding 追加 vs 新 DO クラス追加)

### Q3. Rate limit の閾値 (各層、初期値)

- 質問: production 既定値をいくつにするか?
- 選択肢のテーブル:
  | 層 | 単位 | 提案値 (recommendation) | 緩和したい場合 |
  |---|---|---|---|
  | 層 1 (WAF) | 1 IP あたり `/ws/lobby` 接続 | 30 接続/分 | (Pro 以上に依存) |
  | 層 1 (WAF) | 1 IP あたり `/ws/<room_id>` 接続 | 60 接続/分 | (同上) |
  | 層 2 (Worker token bucket) | 1 IP / LOGIN_LOBBY | 10 trial/分 | env で `LOBBY_LOGIN_RATE_PER_MIN` |
  | 層 2 (Worker token bucket) | 1 handle / LOGIN_LOBBY | 5 trial/分 | env で `LOBBY_LOGIN_RATE_PER_HANDLE_PER_MIN` |
  | 層 3 (KV counter) | 1 IP / GameRoom 起動 | 20 room/分 | env で `ROOM_CREATE_RATE_PER_IP_PER_MIN` |
- recommendation: 初期値は保守的 (推奨表) で staging E2E + 実トラフィック観測後に調整
- env 経由で動的調整可能にする (config.rs 経由、`LOCAL_DEV_ONLY_VARS_KEYS` 範囲外で `SHARED_PUBLIC_VARS_KEYS` 入り)

### Q4. 拒否時のレスポンス / UX

- 質問: 上限超過時にクライアントに何を返すか?
- 選択肢:
  - **Q4-A**: WS upgrade を 503 で拒否 + `Retry-After` ヘッダ (LOGIN_LOBBY なら `LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>`)
  - **Q4-B**: 静かに切断 (1009 Message Too Big 経路と同じ pattern)
  - **Q4-C**: 専用 close code (例: 4000-4999 範囲のカスタムコード)
- recommendation: **Q4-A** + 既存 `LOGIN_LOBBY:incorrect rate_limited` パターン (memory 由来、`server.rs::handle_connection`
  で実装済) を再利用。HTTP path は 429 で `Retry-After` を返す
- 注意: Floodgate native client は `LOGIN:incorrect` を `LOGIN_LOBBY:incorrect` と同様に解釈する仕様か要確認
  (`crates/rshogi-csa-server-workers/src/lobby_protocol.rs::build_login_incorrect_line` 周辺の format)

## 4. 実装計画 (Q1-Q4 確定後の素案)

### 4.1 PR 構成案

1 PR にすると差分が大きいので 2 PR に分割提案:

- **PR3a: 層 2 + 層 3 + runbook** (Worker code-only)
  - `crates/rshogi-csa-server-workers/src/rate_limit.rs` (新規 module、token bucket pure logic)
  - `router.rs` / `lobby.rs` / `game_room.rs` の WS upgrade 経路で `rate_limit::check_*` を呼び込み
  - `ConfigKeys` に閾値 env 追加 (Q3 表参照)
  - `docs/csa-server/rate_limit.md` (runbook、WAF ダッシュボード手順 + 環境変数 reference)
  - host テストで token bucket logic を verify
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

→ Q2-A (KV) 採択ならこの懸念は浮かない。

## 5. Test plan (実装 PR 用 checklist)

- [ ] `crates/rshogi-csa-server-workers/src/rate_limit.rs` の pure helper を host テストでカバー
  (token consume / refill, window 切替, 多 IP 並列)
- [ ] miniflare smoke E2E に rate limit シナリオ追加 (`tests/miniflare_smoke/rate_limit.test.ts`):
  - 同一 IP から N+1 回 LOGIN_LOBBY を投げ、N+1 回目に `LOGIN_LOBBY:incorrect rate_limited` で拒否
  - 同一 IP から M+1 回 `/ws/<room_id>` upgrade で 503 + `Retry-After` レスポンス
  - 1 分後に窓がリセットされて受理される
- [ ] `cargo test -p rshogi-csa-server-workers --lib rate_limit` 全 pass
- [ ] `cargo build --target wasm32-unknown-unknown --lib --release` green
- [ ] staging E2E: 大量 LOGIN flood 試験で全マッチング停止が回避されることを観測
- [ ] runbook (`docs/csa-server/rate_limit.md`) で WAF dashboard 手順 + env tuning gradient を doc 化

## 6. 関連

- 親 issue: [#622](https://github.com/SH11235/rshogi/issues/622)
- 並走 (Session A 同パッケージ):
  - PR [#662](https://github.com/SH11235/rshogi/pull/662) (#560、admin auth foundation) — merged
  - PR [#663](https://github.com/SH11235/rshogi/pull/663) (#621、admin command) — merged
  - [#664](https://github.com/SH11235/rshogi/issues/664) (#621 follow-up、LOGIN handle 自称強化)
- Session C (capacity) との依存: [#632](https://github.com/SH11235/rshogi/issues/632) (LobbyDO sharding)
  と RateLimitDO 設計の重複に注意
- Session B (ops) との依存: [#625](https://github.com/SH11235/rshogi/issues/625) (alerting / metrics)
  で本 PR の rate limit 拒否カウントを観測する経路を別途整備
