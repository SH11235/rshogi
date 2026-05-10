# rshogi-csa-server-workers Rate limit 運用 Runbook

[Issue #622](https://github.com/SH11235/rshogi/issues/622) PR3a で導入した
LOGIN_LOBBY / CHALLENGE_LOBBY / `/ws/<room_id>` upgrade / room create に対する
**Worker code-only** rate limit の運用手順をまとめる。設計判断の根拠は
[`docs/csa-server/rate_limit_design.md`](./rate_limit_design.md) を参照。

> 本 runbook は **層 2 (atomic token bucket)** + **層 3 (room create cap)** を
> 扱う。**層 1 (Cloudflare WAF Rate Limiting Rules)** は IaC
> [#675](https://github.com/SH11235/rshogi/issues/675) Phase 3 と統合する PR3b
> 以降のスコープで、本 runbook の対象外。

## 1. アーキテクチャ概要

```
┌────────────────────────────────────────────────────────────────────┐
│ Cloudflare edge — staging / production の各 worker namespace        │
│                                                                    │
│  Client                                                            │
│   │ /ws/<room_id> + CF-Connecting-IP                               │
│   ▼                                                                │
│  ┌─────────────────────┐                                           │
│  │ Worker (router.rs)  │                                           │
│  │  - Origin / Upgrade │                                           │
│  │  - extract_client_ip│                                           │
│  │  - rate_limit RPC ──┼────►┌────────────────────────┐           │
│  │  - id_from_name     │     │ RateLimiter DO          │           │
│  │  - stub.fetch       │     │ id = "{kind}:{ident}"   │           │
│  └─────────────────────┘     │ - refill (token bucket)│           │
│                              │ - try_consume          │           │
│                              │ - persist (storage)    │           │
│                              └────────────────────────┘           │
│                                                                    │
│  /ws/lobby 経路は CF-Connecting-IP を LobbyDO の Pending attachment │
│  に保存し、LOGIN_LOBBY / CHALLENGE_LOBBY 受信時に同 RateLimiter DO に │
│  per-IP / per-handle / per-inviter の各 bucket で 2 段チェックする。 │
└────────────────────────────────────────────────────────────────────┘
```

- **DO sharding 戦略**: `id_from_name(format!("{kind}:{identifier}"))` で
  per-(kind, identifier) に DO を分散。同 IP / handle のリクエストは決定論的
  に同 instance に到達する。Cloudflare 側で idle DO は自動 GC。
- **token bucket**: capacity = `<env>` per minute、refill rate = capacity / 60
  token/sec。満タンからの burst を許容しつつ、長期平均は X/min を超えない。
- **fail-closed**: `CF-Connecting-IP` 欠落 / DO RPC エラーは **deny** に倒す
  (ヘッダ偽装や transient 障害で全 allow に振れない security-first 既定)。
- **wire format**: 拒否時は `LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>`
  / `CHALLENGE_LOBBY:incorrect rate_limited retry_after=<sec>` /
  `/ws/<room_id>` upgrade では HTTP 503 + `Retry-After: <sec>` ヘッダ
  (design doc Q4-A 採択)。WS は close せず client が retry 可能な状態を保つ。

## 2. 環境変数 reference

`wrangler.<env>.toml` の `[vars]` テーブルで宣言する 6 個の閾値。すべて
`SHARED_PUBLIC_VARS_KEYS` に含まれるため、production / staging 両方の toml で
同名キーを宣言する契約 (CI 整合性 test
[`tests/wrangler_environment_toml_consistency.rs`](../../crates/rshogi-csa-server-workers/tests/wrangler_environment_toml_consistency.rs)
が gate)。

| 環境変数 | 単位 | production / staging 既定 | 対応する `RateLimitKind` |
|---|---|---|---|
| `LOBBY_LOGIN_RATE_PER_IP_PER_MIN` | LOGIN_LOBBY 試行/分 (per IP) | `10` | `LobbyLoginPerIp` |
| `LOBBY_LOGIN_RATE_PER_HANDLE_PER_MIN` | LOGIN_LOBBY 試行/分 (per handle) | `5` | `LobbyLoginPerHandle` |
| `LOBBY_CHALLENGE_RATE_PER_IP_PER_MIN` | CHALLENGE_LOBBY 試行/分 (per IP) | `5` | `LobbyChallengePerIp` |
| `LOBBY_CHALLENGE_RATE_PER_HANDLE_PER_MIN` | CHALLENGE_LOBBY 試行/分 (per inviter) | `3` | `LobbyChallengePerInviter` |
| `ROOM_CREATE_RATE_PER_IP_PER_MIN` | `/ws/<room_id>` (player) upgrade /分 | `20` | `RoomCreatePerIp` |
| `WS_ROOM_UPGRADE_RATE_PER_IP_PER_MIN` | `/ws/<room_id>(/spectate)` upgrade /分 | `60` | `WsRoomUpgradePerIp` |

### 値の解釈

- 各値は **u32 の正値**。`0` / 空 / 非数値 / `u32` 範囲外は安全側既定
  (`crate::rate_limit::RateLimitThresholds::DEFAULTS`) にフォールバック。
  「env で `0` を入れて運用を無効化」という経路は **意図的に提供していない**
  (運用で外したい場合は十分に大きい値 (例: `1000000`) を入れて事実上無効化)。
- 値は **deploy なしで反映** される (Worker fetch ハンドラが env を都度読む
  → DO に capacity を query string で引き渡す設計)。閾値変更は
  `wrangler.<env>.toml` の `[vars]` を編集 → PR で merge → CI deploy で完了。
  Cloudflare secret (`wrangler secret put`) 経由ではない。

## 3. 閾値調整手順

### 3.1 緩和 (cap を上げる)

**典型ケース**: legitimate な高頻度 LOGIN / 大規模イベント運用で false positive 拒否
が発生したとき。

1. Cloudflare Logs / `wrangler tail` で `rate_limit_denied` ログを観測し、
   どの kind / IP / handle が当たっているか特定する (§4 参照)
2. `wrangler.<env>.toml` の該当 env を 1.5〜2x 程度に増やす (e.g. `10` → `15`)
3. 通常の PR レビュー経路で merge (本 PR 同様 Codex review を回す)
4. CI deploy 完了後、`wrangler tail` で `rate_limit_denied` 件数の減少を観測

### 3.2 厳格化 (cap を下げる)

**典型ケース**: 攻撃 / 異常 traffic を検知して短期に絞りたいとき。

1. **緊急時 hotfix path**: 直接 production wrangler.toml を編集 → PR merge
   → CI deploy (rollout は約 1〜2 分)
2. 逆に過剰拒否で legitimate user が締め出される場合は §3.1 で緩和

> **注意**: 閾値を下げ過ぎると正規ユーザの再接続失敗が増えて support 負荷
> が上がる。staging で実 traffic ベースライン (`rate_limit_denied` の
> baseline 件数) を計測してから production に反映するのが安全。

### 3.3 staging で先行検証

新しい閾値の挙動を確認する場合:

1. `wrangler.staging.toml` のみ先に変更 → PR merge → staging 自動 deploy
   (main への push trigger)
2. `csa-e2e-staging` skill (`docs/csa-client/staging.md` 参照) で 7 シナリオ
   通電
3. 問題なければ production 側 toml も同値に揃える PR を別途 merge

## 4. 観測 / debug

### 4.1 構造化ログイベント

すべて [`structured_log!`](../../crates/rshogi-csa-server-workers/src/observability.rs)
macro 経由で 1 行 JSON で出力 ([Issue #625](https://github.com/SH11235/rshogi/issues/625)
Phase A の流儀に揃える)。`event` フィールドが alert / aggregation の primary 軸。

| event | component | 出力タイミング | 主要 field |
|---|---|---|---|
| `rate_limit_missing_cf_ip` | `router` / `lobby` | CF-Connecting-IP 欠落で fail-closed | `path` / `kind` / `handle` / `inviter` |
| `rate_limit_denied` | `router` / `lobby` | bucket 超過で reject | `kind`, `ip`, `handle`/`inviter`, `retry_after_sec`, `path` |
| `websocket_upgrade_accepted` | `lobby` | LobbyDO upgrade 成功 (rate limit pass 後の通常経路) | `cf_ip_present` (rate limit 観測補助) |

### 4.2 grep 例 (Cloudflare Tail Workers / R2 archive)

```bash
# 直近 1 時間の rate_limit denial を kind 別に集計
wrangler tail --config wrangler.production.toml --format json \
  | jq 'select(.event == "rate_limit_denied") | .kind' \
  | sort | uniq -c

# 特定 IP に対する denial を全 kind 横断で抽出
wrangler tail --config wrangler.production.toml --format json \
  | jq 'select(.event == "rate_limit_denied" and .ip == "203.0.113.50")'

# CF-Connecting-IP 欠落 (本来ありえない anomaly) の検知
wrangler tail --config wrangler.production.toml --format json \
  | jq 'select(.event == "rate_limit_missing_cf_ip")'
```

R2 archive 経路 ([Issue #625](https://github.com/SH11235/rshogi/issues/625)
Phase B 統合後) では `floodgate-history/YYYY/MM/DD/...` と同様のキー体系で
保存される logs に対して同 jq query を回せる。

### 4.3 `wrangler tail` 一時起動 (Tail Workers 不在時)

[`docs/csa-server/deployment.md`](./deployment.md) の §5 参照。

```bash
cd crates/rshogi-csa-server-workers
vp run tail:prod          # production
vp run tail:staging       # staging
```

## 5. トラブルシュート

### 5.1 すべての client が `rate_limited` で reject される

- **原因 1**: env 値の typo / `0` / 範囲外で fallback (= 既定値) に倒れた
  + 既存 traffic が常に default cap を超えている
  - 対処: `wrangler.<env>.toml` の値を確認し、適正値に戻す
- **原因 2**: `CF-Connecting-IP` が Cloudflare edge 経由で来ていない
  (proxy 設定不正 / Origin direct hit)
  - 対処: `rate_limit_missing_cf_ip` イベントの頻度を確認、
    Cloudflare ダッシュボードで proxy 状態を確認
- **原因 3**: 同 NAT 配下の多 client が同 IP として観測されて per-IP cap を
  共有してしまう (= NAT 同居問題)
  - 対処: per-handle cap を相対的に緩める or 当該 IP の per-IP cap を運用
    例外的に上げる (本 PR では IP 単位例外設定機構は提供しない、必要時
    follow-up issue で扱う)

### 5.2 RateLimiter DO storage の成長モデル

**重要 — DO storage は永続的**: Cloudflare Durable Object は「**isolate (in-memory
state) は idle で evict** される」が、「**`state.storage()` の永続データは明示
削除しない限り残る**」(公式仕様)。本 PR3a では `state.storage().put` を毎 check 後
に呼ぶ設計のため、過去に touch された全 (kind, identifier) ペアの bucket state が
storage に残る。

**実測上の storage 消費**:

| シナリオ | unique (kind, identifier) 数 | storage 消費 |
|---|---|---|
| 単一 IP × 6 bucket kinds | 6 | ~96 byte |
| 1,000 unique IP × 6 kinds | 6,000 | ~96 KB |
| 1M unique IP × 6 kinds (botnet 級) | 6M | ~96 MB |

5 GB Free 枠に対して、1M unique IP までは余裕がある (Floodgate 想定 traffic では
ここまで来ない)。**懸念**: CF-Connecting-IP を変えながら攻撃する分散ボットネットが
来た場合、storage が数 GB 規模まで成長する可能性がある (理論上限)。

**現状の運用**: 本 PR3a では TTL ベースの自動 storage 削除を実装しない
(scope minimization)。以下の状況になったら follow-up issue で alarm-based cleanup
を追加する:

- DO 「Storage Used」が 1 GB を超える
- 月次の DO storage コストが想定外に増える

**follow-up 候補設計**: `RateLimiter::fetch` で persist 後に DO Alarm を
`last_refill_ms + 60_000` で予約。Alarm 発火時に bucket が満タン
(= しばらく未使用) なら `state.storage().delete_all()` で破棄する。次回 fetch で
fresh start。これは別 PR スコープ。

**確認手順**: Cloudflare ダッシュボードの「Workers & Pages」→ 該当 worker →
「Settings」→「Durable Objects」で class 別 instance 数 / storage 使用量を確認する。
急増があれば本 §5.2 follow-up を起動する判断材料にする。

### 5.3 Miniflare 上で `CF-Connecting-IP` 欠落の挙動を再現したい

**できない**: Miniflare 4 は edge proxy をシミュレートするために
`CF-Connecting-IP` を **常に** request 元の IP で自動注入する。本シナリオ
(production で Cloudflare を bypass されるケース) は host pure unit test
(`crates/rshogi-csa-server-workers/src/rate_limit.rs::tests`) と
[`tests/miniflare_smoke/rate_limit.test.ts`](../../crates/rshogi-csa-server-workers/tests/miniflare_smoke/rate_limit.test.ts)
内コメントで担保している (= 直接 smoke にできない理由を doc 化)。

実 production では Cloudflare edge を経由しない経路は存在しないため、
本シナリオは **理論上のみ** の fail-closed safety net。

### 5.4 closed loop testing: rate limit が効いているか手動確認したい

```bash
# CF-Connecting-IP を固定して 11 回連続 LOGIN_LOBBY (= 既定 10/分 を 1 超過)
for i in $(seq 1 11); do
  curl -sS \
    -H "Upgrade: websocket" \
    -H "CF-Connecting-IP: 203.0.113.99" \
    -H "Sec-WebSocket-Version: 13" \
    -H "Sec-WebSocket-Key: $(openssl rand -base64 16)" \
    "https://rshogi-csa-server-workers.example/ws/lobby"
  echo " ... attempt $i"
done
```

11 回目以降の試行で `503 Service Unavailable` + `Retry-After: <sec>` ヘッダが
返れば rate limit が機能している (実 LOGIN_LOBBY message を流すには
WebSocket library が必要なので、上記は upgrade レイヤのみの確認)。

> **注意**: 本確認を production に流すと自分自身が rate limit に当たる。
> staging で行うか、安全な別 IP / VPN 経由で行う。

## 6. 関連

- 設計 doc: [`docs/csa-server/rate_limit_design.md`](./rate_limit_design.md)
- 親 issue: [#622](https://github.com/SH11235/rshogi/issues/622)
- 実装: [`crates/rshogi-csa-server-workers/src/rate_limit.rs`](../../crates/rshogi-csa-server-workers/src/rate_limit.rs)
- 配線: [`router.rs::forward_ws_to_room`](../../crates/rshogi-csa-server-workers/src/router.rs) /
  [`lobby.rs::handle_login_lobby`](../../crates/rshogi-csa-server-workers/src/lobby.rs)
- DO migration 契約: [`docs/csa-server/deployment.md`](./deployment.md) §3.4
- 観測連携 (alerting / R2 archive): [Issue #625](https://github.com/SH11235/rshogi/issues/625)
- WAF / IaC 統合 (= PR3b): [Issue #675](https://github.com/SH11235/rshogi/issues/675)
  Phase 3
- client 互換性 (`retry_after` honoring): [#682](https://github.com/SH11235/rshogi/issues/682)
  / [#683](https://github.com/SH11235/rshogi/pull/683)
