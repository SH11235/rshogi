# rshogi-csa-server-workers Observability Runbook

[Issue #625](https://github.com/SH11235/rshogi/issues/625) で整備する 24/7 無人運用基盤の運用手順。

- **Phase A** (✅ 完了 [PR #691](https://github.com/SH11235/rshogi/pull/691)): `structured_log!` macro 導入、全 `console_log!` を JSON 化
- **Phase B** (🟡 部分完了 [PR #698](https://github.com/SH11235/rshogi/pull/698) + [issue #700](https://github.com/SH11235/rshogi/issues/700) を [PR #701](https://github.com/SH11235/rshogi/pull/701) で本 doc 訂正): Cloudflare Notifications → Slack webhook 経路は実機検証済 (✅)。Workers Logs → R2 archive 経路は **Workers Free plan で利用不可** が判明し延期 (Paid plan 移行時に再活性化、§7 参照)
- **Phase C** (✅ 完了 [PR #671](https://github.com/SH11235/rshogi/pull/671) で [#630](https://github.com/SH11235/rshogi/issues/630) と統合): synthetic monitoring

> **Workers Free plan 制約 (本 doc の前提)**: rshogi-csa-server-workers は 2026-05-10 時点で Workers Free plan で運用しており、`workers_trace_events` dataset の Logpush は API レベルで `code 1004: exceeded max jobs allowed` で reject される (Free plan は 0 job 許可)。本 doc は Free plan 前提で書かれている。Paid plan 移行時は §7 に従って Logpush + R2 archive 経路を再活性化する。

## 1. アーキテクチャ概要 (現状: Free plan)

```
┌─────────────────────────────────────────────────────────────┐
│ Cloudflare Worker (rshogi-csa-server-workers)               │
│   ↓ structured_log!() macro (Phase A、JSON 形式)              │
│ Workers console output (短期、wrangler tail で観察可能)        │
└─────────────────────────────────────────────────────────────┘
         (現状は archive 経路なし、wrangler tail で real-time のみ)

┌─────────────────────────────────────────────────────────────┐
│ Cloudflare Notifications (account-level)                    │
│   - 配信先 webhook (rshogi-{stg,prod}-alerts) 登録済 ✅          │
│   - NotificationPolicy (workers_observability_alert) ⚠️       │
│     staging/production API 直作成済だが **silent state**:     │
│     Cloudflare 公式 alert rule UI/API が public release 前、   │
│     発火経路なし (§6 参照)。当面は health_check_status_       │
│     notification を本命とする (別 PR で実装)                  │
└────────┬────────────────────────────────────────────────────┘
         │ POST request (Cloudflare-format JSON payload、Cloudflare が
         │ Slack 形式と判別して dispatch する type=slack 設定)
         ↓
┌─────────────────────────────────────────────────────────────┐
│ Slack incoming webhook (rshogi-cloudflare-alerts App)       │
│   ※ Discord 切替時は translator Worker (別 PR) 経由           │
└─────────────────────────────────────────────────────────────┘
```

**Paid plan 移行後の追加構成** (§7 で詳細):

```
Workers console output ↓ Logpush (NDJSON、30 秒 batch、enabled flag で gate)
                       ↓
                       R2 bucket: rshogi-csa-logs-{staging,prod}
                       ↓
                       (任意) NotificationPolicy: failing_logpush_job_disabled_alert
```

## 2. IaC リソース現状 (2026-05-10 時点)

| Resource 種別 | 名前 (production) | 名前 (staging) | 状態 | Pulumi 配置 |
|---|---|---|---|---|
| R2 bucket (logs archive) | `rshogi-csa-logs-prod` | `rshogi-csa-logs-staging` | ✅ 作成済 (Free plan では空、Paid 移行時に Logpush 投入先) | `infra/pulumi/index.ts` |
| NotificationPolicyWebhooks | (未作成) | `rshogi-staging-alerts` (id `e9e6102c...`) | ✅ staging のみ作成済、Slack 疎通確認済 | `infra/pulumi/index.ts` |
| LogpushJob | – | – | ❌ Free plan で作成不可、config 投入で declare をスキップ | `infra/pulumi/index.ts` (scaffold 維持) |
| NotificationPolicy `workers_observability_alert` 用 | `rshogi-production-workers-observability` (id `bd74a99e254b4e7585b6387bb58bad87`) | `rshogi-staging-workers-observability` (id `23eb8141856748a3bf42094da6b3a1c4`) | ⚠️ **silent state**: API 直作成済 (2026-05-10) だが Cloudflare 公式 alert rule UI/API が public release 前で発火経路なし。残置は副作用なし、将来 Cloudflare 公開時に流用可能 (§6 参照) | (Pulumi 不可) |
| NotificationPolicy `logpushFailureAlert` | – | – | ❌ Free plan で logpushJob 不在のため依存 chain skip、Paid 移行時に自動 active | `infra/pulumi/index.ts` (scaffold 維持) |

**Pulumi scaffold 設計**: `infra/pulumi/index.ts` の `readOptionalSecret(key)` ヘルパーで「config 値が unset または空文字列なら resource 自体を declare しない」条件分岐を持たせており、Free plan では Logpush 関連 config を投入しないことで自動的にスキップされる。Paid plan 移行時は §7.1 に従って config 投入のみで Logpush + alert を再活性化できる。

## 3. Bootstrap 完了履歴 (Phase B 初回投入、2026-05-10)

> **本節は履歴記録**。再 bootstrap が必要な状況 (新規 Cloudflare アカウントへの移植 / 既存 Pulumi state 喪失 / production stack 追加) でのみ参照する。

### 3.1 完了済ステップ

| # | 作業 | 結果 |
|---|---|---|
| 3.1.1 | `pulumi-rshogi-iac` token に `Account: Notifications: Edit` scope 追加 (user manual) | ✅ scope 追加完了。`Account: Logs: Edit` は Free plan で unused だが Paid 移行時に活性化する想定で keep |
| 3.1.2 | Slack workspace に `rshogi-cloudflare-alerts` App 作成 + Incoming Webhook を target channel に install (user manual) | ✅ Webhook URL 取得済 |
| 3.1.3 | R2 bucket `rshogi-csa-logs-staging` を Pulumi で create (`pulumi up`) | ✅ bucket 作成済 (空) |
| 3.1.4 | Pulumi config 投入: `alertWebhookName`, `alertWebhookUrl` (--secret) | ✅ 投入済 |
| 3.1.5 | `pulumi up` で `NotificationPolicyWebhooks` を create | ✅ id `e9e6102c...`、type=slack で作成 (Cloudflare が URL pattern から Slack 形式を自動検出) |
| 3.1.6 | Slack 疎通確認 | ✅ Cloudflare からの test message が rshogi-cloudflare-alerts channel に届くこと確認 |

### 3.2 試行したが失敗したステップ (記録)

| # | 作業 | 失敗原因 | 対処 |
|---|---|---|---|
| 3.2.1 | `LogpushJob` (workers_trace_events dataset) を create | Cloudflare API `code 1004: exceeded max jobs allowed` (Workers Free plan は 0 job 許可) | Logpush 関連 config を `pulumi config rm` で削除して LogpushJob declare 自体をスキップ。Paid 移行時に再活性化 (§7.1) |
| 3.2.2 | `NotificationPolicy logpushFailureAlert` (alertType=failing_logpush_job_disabled_alert) を create | LogpushJob 不在で alert 対象がない (依存 chain で Pulumi が自動 skip) | Logpush 非依存の alert は公式 GA の `health_check_status_notification` を採用予定 (§6.1 参照、別 PR で declare)。`workers_observability_alert` も最初本命に置いたが公式 doc 未登録と判明し未採用に降格 (§6.2 参照) |
| 3.2.3 | `pulumi config set --secret alertWebhookSecret <random>` を投入 | Cloudflare Notifications API が "secret field は URL embedded secret (PagerDuty 形式) との一致検証用、Slack URL には不要" と reject (`malformed request: url formatting error`) | `pulumi config rm alertWebhookSecret` で削除。Slack 直結時は secret 不要、Discord translator Worker 経由時は Worker 内で `cf-webhook-auth` header と独自 HMAC 検証する設計に変更 (§5) |
| 3.2.4 | `wrangler.{production,staging}.toml` に `logpush = true` を keep (PR #698 で追加、PR #704 直前まで「Free plan で no-op」と誤解) | Workers Free plan は `logpush` フラグ自体を deploy gate で reject、`A request to the Cloudflare API .../workers/scripts/{name} failed. You do not have access to use Logpush. [code: 10023]` で **deploy job が error 終了 (3 連続失敗)** | PR #704 post-merge fixup で両 toml の該当行をコメントアウト。Paid plan 移行後に有効化する手順は §7.3 参照 |

### 3.3 再 bootstrap 時の手順 (新環境向け)

新規環境で同等構成を作る場合の手順:

```bash
cd infra/pulumi
pulumi stack select staging  # or production

# 1. R2 bucket を作成 (Free plan でも作成は可能)
#    declare scaffold は config 投入なしでも bucket だけ create する設計
pulumi up

# 2. Notifications webhook 作成
#    平文 (Cloudflare Dashboard 表示名)
pulumi config set alertWebhookName 'rshogi-staging-alerts'

#    secret 値: --secret のみ指定 → 対話 prompt で stdin 入力 (shell history に残らない)
pulumi config set --secret alertWebhookUrl
# (Slack incoming webhook URL を貼り付け → Enter)

# 3. apply で webhook を Cloudflare 上に作成
pulumi preview  # NotificationPolicyWebhooks 1 件の create を確認
pulumi up

# 4. Slack 疎通確認
#    Cloudflare API で webhook 一覧を確認
ACCOUNT_ID="d5d9818649d8722f73cd798c3b1ffb70"
TOKEN=$(pulumi config --show-secrets --json | jq -r '.["cloudflare:apiToken"].value')
curl -sS "https://api.cloudflare.com/client/v4/accounts/$ACCOUNT_ID/alerting/v3/destinations/webhooks" \
  -H "Authorization: Bearer $TOKEN" \
  | jq '.result | map({id, name, type, last_success, last_failure})'

#    Cloudflare Dashboard で test notification 送信 (or 後述 §3.4 の curl)
```

### 3.4 secret 漏洩防止 pattern (重要)

`pulumi config set --secret KEY 'value'` の **shell 引数渡しは禁止** (`~/.bash_history` / `~/.zsh_history` に値が残る、`--secret` フラグは Pulumi state 上の暗号化のみで shell history 漏洩は防がない)。

| 投入方法 | 用途 | コマンド例 |
|---|---|---|
| **対話 prompt** (stdin、echo 抑止) | 単一の短い値 (URL 等) | `pulumi config set --secret KEY` |
| **pipe** (生成器から直接) | random 生成値 | `openssl rand -hex 32 \| pulumi config set --secret KEY` |
| **file redirect** (`< /tmp/file`、削除込み) | 長い multi-line 値 | `umask 077 && cat > /tmp/v && pulumi config set --secret KEY < /tmp/v && shred -u /tmp/v` |

shell history 既汚染時は: `history -d <line>` + `history -c && history -w` で消去 + secret 値を rotation (発行元で再発行)。

## 4. ログ検索 / 調査運用 (Free plan)

### 4.1 リアルタイム tail (現状の主経路)

`wrangler tail` は Cloudflare Workers の console 出力を websocket で stream する。出力は **request invocation 単位の envelope JSON** で:

```json
{
  "outcome": "ok",
  "scriptName": "rshogi-csa-server-workers",
  "event": { "request": {...}, ... },   ← request metadata (structured_log の event とは別物)
  "logs": [
    { "message": ["{\"event\":\"room_join\",\"ts_ms\":...,\"game_id\":\"...\"}"], "level": "log", "timestamp": ... },
    { "message": ["plain string log"], "level": "log", ... }
  ],
  "exceptions": [ ... ]
}
```

`structured_log!` macro が出した JSON 文字列は **`logs[].message[]` 配列の要素** として埋め込まれる。**top-level の `event` フィールドは request metadata** であって構造化ログの `event` フィールドではないので、`select(.event != null)` ではフィルタにならない (毎 invocation で truthy になる)。

正しい抽出は §7.1.4 と同じく `fromjson?` で JSON message のみ展開:

```bash
# 1 行 1 invocation envelope → logs[].message を展開して 1 行 1 structured_log にフラット化
wrangler tail rshogi-csa-server-workers --format json \
  | jq -c '.logs[]?.message[]? | fromjson? // empty'

# 特定 event だけフィルタ (例: room_join のみ)
wrangler tail rshogi-csa-server-workers --format json \
  | jq -c '.logs[]?.message[]? | fromjson? // empty | select(.event == "room_join")'

# 特定 game_id を時系列で (リアルタイム stream なので "時系列" は到着順)
wrangler tail rshogi-csa-server-workers --format json \
  | jq -c '.logs[]?.message[]? | fromjson? // empty | select(.game_id == "<game_id>")'

# staging 側
wrangler tail rshogi-csa-server-workers-staging --format json \
  | jq -c '.logs[]?.message[]? | fromjson? // empty'
```

> `fromjson?` (`?` は jq のエラー抑制) で plain string message (panic / 通常 console.log) はスキップされ、`structured_log!` 由来の JSON だけが残る。`// empty` で fromjson 失敗時 (null) を行ごと捨てる。

**制約**: `wrangler tail` は **接続中の event のみ受信** する (過去ログは見えない)。障害発生後の遡及調査には使えない。これが必要な場合は §7.1 で Paid plan に移行して Logpush + R2 archive を有効化する。

### 4.2 Cloudflare Dashboard UI の実態 (2026-05 時点、API fallback 推奨)

Cloudflare 新 UI (`https://dash.cloudflare.com/<account_id>/notifications`) は **「すべての通知 / All Notifications」タブ 1 枚のみ** で、destinations (webhook 一覧) を独立に表示・管理する画面が提供されていない。`/notifications/destinations` URL は `/notifications` に redirect される。

実観測 (2026-05-10):

- 「アカウントの管理 → 通知」ページ表示
- 左側に「すべての通知」タブのみ
- 中央エリアに「Cloudflare アカウントの通知を作成します」と「追加」ボタン (= 新規 notification policy 作成 wizard を起動)
- **destinations 一覧 / webhook 編集 UI は無し**
- policy 作成 wizard 内で既存 destination から選択 or 新規作成する fluent 経路に集約された

つまり webhook destinations の管理は **policy wizard 内 + API のみ**。standalone destination 編集が必要なら API で直接操作する。

```bash
ACCOUNT_ID="d5d9818649d8722f73cd798c3b1ffb70"
TOKEN=$(pulumi config --show-secrets --json | jq -r '.["cloudflare:apiToken"].value')

# webhook destinations 一覧
curl -sS "https://api.cloudflare.com/client/v4/accounts/$ACCOUNT_ID/alerting/v3/destinations/webhooks" \
  -H "Authorization: Bearer $TOKEN" | jq '.result | map({id, name, type, last_success, last_failure})'

# notification policies 一覧
curl -sS "https://api.cloudflare.com/client/v4/accounts/$ACCOUNT_ID/alerting/v3/policies" \
  -H "Authorization: Bearer $TOKEN" | jq '.result | map({id, name, alert_type, enabled})'

# 本 account / plan で使える alertType 一覧 (新 NotificationPolicy 設計時に参照)
curl -sS "https://api.cloudflare.com/client/v4/accounts/$ACCOUNT_ID/alerting/v3/available_alerts" \
  -H "Authorization: Bearer $TOKEN" \
  | jq '.result | to_entries | map({category: .key, alerts: (.value | map({type, display_name}))})'
```

本リポでは **Pulumi で declare → 確認は API call** で完結する運用なので、Dashboard UI の制約は実害なし。

### 4.3 Workers built-in observability (Cloudflare Dashboard、7 日保持)

`wrangler.toml` に `[observability]` block を追加すると Cloudflare Dashboard の Workers script 詳細画面で過去 7 日分の log を検索できる (Free plan でも利用可能、本 PR では未設定)。

将来必要になったら以下を追加:

```toml
# crates/rshogi-csa-server-workers/wrangler.{production,staging}.toml
[observability]
enabled = true

[observability.logs]
enabled = true
head_sampling_rate = 1.0
```

これは Logpush とは別経路 (Cloudflare 内部の log buffer)。Free plan でも 7 日保持される一方、archive を S3 / R2 に流すには Logpush が必要 (= Paid plan)。

## 5. Discord 切替方針 (将来)

Cloudflare Notifications の webhook destination は **URL pattern から自動的に dispatch 形式を決定** する (例: `hooks.slack.com` なら Slack 形式 payload を送る、`type=slack` として保存される)。Discord webhook (`discord.com/api/webhooks/...`) は native 形式 (`{"content": ...}` or `{"embeds": [...]}`) を期待し、Cloudflare が送る `{name, text, data, ts, policy_id, account_id}` 形式と互換性がない。

**translator Worker** (~50 行の Cloudflare Workers script) を 1 枚追加することで Discord (or 他チャネル) に乗換可能:

```ts
// 簡略例 (将来 PR で実装)
export default {
  async fetch(req: Request, env: Env) {
    const cfPayload = await req.json();
    const discordPayload = {
      content: `**${cfPayload.name}**\n${cfPayload.text}`,
      embeds: [{ description: JSON.stringify(cfPayload.data, null, 2) }],
    };
    return fetch(env.DISCORD_WEBHOOK_URL, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(discordPayload),
    });
  },
};
```

切替手順 (translator Worker deploy 後):

```bash
# §3.4 と同じく shell 引数経由は禁止 (history 漏洩)。--secret のみ指定して
# 対話 prompt で stdin 入力する。
pulumi config set --secret alertWebhookUrl
# (translator Worker URL を貼り付け → Enter、prompt は echo されないので shell history には残らない)
pulumi up
```

`NotificationPolicyWebhooks` の `url` のみ差し替わる (Cloudflare は新 URL pattern から `type` を再判定、Discord の場合 generic webhook として扱われる)。HMAC 検証する場合は translator Worker 内で `cf-webhook-auth` header を別途検証する (Cloudflare が自動で付ける header、Pulumi config の `alertWebhookSecret` は Slack 直結用途では使えなかった (§3.2.3 参照) ので translator Worker 側に独自 secret を持たせる設計とする)。

## 6. Free plan で実用的な alert 追加方針

`/available_alerts` API を Free plan + 本アカウントで叩いた結果 (2026-05-10) + 公式 [Available Notifications docs](https://developers.cloudflare.com/notifications/notification-available/) + [Alerting API docs](https://developers.cloudflare.com/api/resources/alerting/) と突き合わせた結果 (2026-05-11)、本 use case (Worker 監視) で利用可能な alertType は以下:

| alertType | カテゴリ | 公式 doc | Worker での実用性 |
|---|---|---|---|
| `health_check_status_notification` | Health Checks | ✅ 公式 docs あり、GA | ✅ **本命**: Cloudflare Health Check リソース (HTTP probe `/health` 等) を別途 setup → DOWN 検知で Slack 通知。Free plan 利用可、公式 release 済 (§6.1 参照、別 PR で実装予定) |
| `workers_observability_alert` | Workers Observability | ❌ 公式 docs **未登録** (70+ alertType 一覧に無し) | ⚠️ **silent / 未採用**: API endpoint は存在、`/available_alerts` で返却、NotificationPolicy 作成可だが、**alert rule 定義経路が public release 前** (Dashboard UI / API 共に未公開、§6.2 参照)。staging/production NotificationPolicy 残置 (発火しないが副作用なし、Cloudflare 公開後流用可) |
| `security_insights_alert` | Security Insights | ✅ 公式 docs あり | △ Worker specific ではないが zone 全体の脅威検知として受信して損なし (本 doc では別 PR で評価) |
| `real_origin_monitoring` | Traffic Monitoring | ✅ 公式 docs あり | ❌ Worker には適用されない (zone-attached origin 専用) |

**`dos_attack_l7` 等の DoS Protection alertType は Free plan の `/available_alerts` 結果に出現せず**、Free plan は基本 DDoS 保護のみで configurable alert は Paid 限定 (2026-05-10 検証で判明)。

### 6.1 `health_check_status_notification` (本命、別 PR で実装予定)

公式 [Notifications docs](https://developers.cloudflare.com/notifications/notification-available/) + [Alerting API docs](https://developers.cloudflare.com/api/resources/alerting/) に記載のある **GA 機能**。Workers Free plan 上で uptime 監視を実装する現実解。

**設計概要 (詳細は別 PR で起票・実装):**

1. Pulumi で `cloudflare.HealthCheck` resource を declare (HTTP probe で `https://rshogi-csa-server-workers.sh11235.workers.dev/health` 等を 1 分間隔で probe)
2. Pulumi で `cloudflare.NotificationPolicy` (alertType=`health_check_status_notification`) を declare、`filters.healthCheckId` に上記 Health Check の id を指定、`mechanisms.webhooks` で既存 `alertWebhook` (rshogi-{stg,prod}-alerts) へ routing
3. これで Worker `/health` endpoint が DOWN を検知 → Slack 通知 が完成

**Pulumi provider 対応状況**: `@pulumi/cloudflare` v6.15.0 で `NotificationPolicy.alertType` の Available values list に `health_check_status_notification` を含む (workers_observability_alert と異なり、IaC declare 可能)。

**含まないもの (このシンプル設計の限界):**
- HTTP error rate (例: 5xx > N%) ベースの alert: Health Check は 200/non-200 の二値判定のみ
- レイテンシ閾値ベースの alert: 同上
- log 内容ベースの alert (panic / specific event): Workers Observability dashboard 経由 (現状 §6.2 silent)

本 doc 範囲外、別 PR (Phase B 続編) で実装する。

### 6.2 `workers_observability_alert` (未採用、Cloudflare 公式 release 待ち)

> ⚠️ **公式 release 前の undocumented feature**。2026-05-10 / 2026-05-11 検証で以下が判明し、本命採用から取り下げ。NotificationPolicy 2 件は残置 (silent state、副作用なし、将来 Cloudflare 公開時に流用可能)。

**未採用の根拠 (2026-05-11 公式 doc 突き合わせ結果):**

| 確認項目 | 結果 |
|---|---|
| [公式 Alerting API docs](https://developers.cloudflare.com/api/resources/alerting/) の 70+ alertType に記載 | ❌ なし |
| [公式 Notifications docs](https://developers.cloudflare.com/notifications/notification-available/) の Workers 関連 | ❌ Workers Usage Notifications (別系統、weekly summary + CPU spike) のみ |
| 公式 changelog (〜 2026-05-07) に Workers Observability alert announcement | ❌ なし |
| `/available_alerts` API での返却 | ✅ あり (実機検証済) |
| `filters.status: ["FIRING_FAILED", "NORMAL"]` 公式仕様 | ⚠️ `/available_alerts` 由来のみ、公式 doc は `health_check_status_notification` 専用 spec として記述 |
| Dashboard UI に alert rule 作成画面 | ❌ user 確認で無し (Worker > Observability tab はクエリ UI のみ、Save as alert ボタン等なし) |
| Pulumi `@pulumi/cloudflare` v6.15.0 alertType enum 収録 | ❌ 未収録 ([PR #704](https://github.com/SH11235/rshogi/pull/704) Codex review で確認) |

= Cloudflare 内部で alertType は予約済だが alert rule 定義経路 (UI / API 共) が **public release 前**。我々が作った NotificationPolicy 2 件は **永久に silent** (rule 定義経路がない状態では発火しない)。

**Cloudflare release 待ちのため残置している resource (2026-05-10〜11 作成):**

| 環境 | NotificationPolicy id | name | 状態 |
|---|---|---|---|
| staging | `23eb8141856748a3bf42094da6b3a1c4` | `rshogi-staging-workers-observability` | silent (alert rule なし、enabled: true) |
| production | `bd74a99e254b4e7585b6387bb58bad87` | `rshogi-production-workers-observability` | silent (同上) |

**`wrangler.{production,staging}.toml` の `[observability]` block も残置**: Workers Observability の Dashboard 機能 (Logs / Query Builder、Free plan で 7 日保持) は public GA なので有効化したまま運用可能 (Phase A の `structured_log!` JSON を Dashboard 上で検索する用途で実利あり)。

**Cloudflare 側 release を確認する trigger:**

1. [公式 Alerting API docs](https://developers.cloudflare.com/api/resources/alerting/) の alertType 一覧に `workers_observability_alert` が登場
2. [Cloudflare changelog](https://developers.cloudflare.com/changelog/) で "Workers Observability alerts" announcement
3. Worker > Observability tab に **"Alerts" / "Create alert rule"** UI が出現

いずれか確認できたら、本節の手順 (旧 §6.1) を更新して本命採用に戻す PR を起票する。

#### Historical record: 2026-05-10〜11 に実施した bootstrap 手順

参考のため、当時 user manual で実行した curl と filter spec の発見を残す (再採用時にそのまま使える):

```bash
# 2026-05-10 staging で実行 (Pulumi v6.15.0 alertType enum 未収録のため API 直作成)
ACCOUNT_ID="d5d9818649d8722f73cd798c3b1ffb70"
TOKEN=$(cd /path/to/rshogi/infra/pulumi && pulumi config --show-secrets --json | jq -r '.["cloudflare:apiToken"].value')
WEBHOOK_ID="e9e6102cf9d64192b5c2443dd70ec9f8"  # rshogi-staging-alerts

curl -sS -X POST "https://api.cloudflare.com/client/v4/accounts/$ACCOUNT_ID/alerting/v3/policies" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg wid "$WEBHOOK_ID" '{
    name: "rshogi-staging-workers-observability",
    description: "Workers Observability dashboard で定義した custom alert rule が発火 (FIRING_FAILED) または回復 (NORMAL) した時に Slack へ routing。",
    alert_type: "workers_observability_alert",
    enabled: true,
    filters: { status: ["FIRING_FAILED", "NORMAL"] },
    mechanisms: { webhooks: [{ id: $wid }] }
  }')"
```

実機検証で判明した API 仕様 (公式 doc に未記載、`/available_alerts` 経由で確認):
- `filters.status` 必須 (`FIRING_FAILED` / `NORMAL`)、空だと `code 17103`
- `alert_interval` は本 alertType で non-supported、含めると `code 17009`


## 7. Paid plan 移行時の手順

[Workers Paid plan ($5/month)](https://dash.cloudflare.com/?to=/:account/workers/plans) に upgrade すると `workers_trace_events` Logpush が利用可能になる (100k events/day 込み、超過分は従量課金)。upgrade 後の手順:

### 7.1 Logpush + R2 archive を活性化

#### 7.1.1 R2 access key 発行 (user manual)

1. https://dash.cloudflare.com/?to=/:account/r2/api-tokens → "Create API Token"
2. **Token name**: `logpush-rshogi-csa-logs`
3. **Permissions**: `Object Read & Write`
4. **Specify buckets**: `rshogi-csa-logs-staging` + `rshogi-csa-logs-prod` の 2 件 (Apply to specific buckets only)
5. **TTL**: 未設定 (年 1 review)
6. 発行後、`Access Key ID` と `Secret Access Key` を **その場でコピー** (二度と表示されない)

#### 7.1.2 Pulumi config 投入

```bash
cd infra/pulumi
pulumi stack select staging  # production は別途同手順

# Logpush destination URL (R2 access key + secret embedded、長い multi-line)
umask 077
cat > /tmp/logpush-destconf <<'DESTEOF'
r2://rshogi-csa-logs-staging/?account-id=<ACCOUNT_ID>&access-key-id=<ACCESS_KEY_ID>&secret-access-key=<SECRET_ACCESS_KEY>
DESTEOF
# <ACCOUNT_ID> = d5d9818649d8722f73cd798c3b1ffb70
# <ACCESS_KEY_ID> / <SECRET_ACCESS_KEY> を §7.1.1 で発行した値で書き換え
pulumi config set --secret logpushDestinationConf < /tmp/logpush-destconf
shred -u /tmp/logpush-destconf

# 初期は disabled で declare のみ → 動作確認後に enable
pulumi config set logpushEnabled false

pulumi preview  # LogpushJob 1 件の create を確認
pulumi up
```

#### 7.1.3 Logpush enable + R2 archive 確認

```bash
pulumi config set logpushEnabled true
pulumi up
# → 30 秒以内に LogpushJob が R2 bucket に書き込み始める

wrangler r2 object list rshogi-csa-logs-staging --remote
# → 1 件以上 NDJSON (.log.gz) object が出てくれば logs 流れ始め
```

#### 7.1.4 R2 archive のログ検索 (Paid plan 後の主経路)

Logpush archive (NDJSON) の各行は `workers_trace_events` の **wrapper object** で、`structured_log!` macro が出した `{event, ts_ms, game_id, ...}` 形式の構造化ログは **`Logs[].Message` 内の文字列** として埋め込まれる (Logs は Worker 1 invocation 内の console 行配列)。`event` / `game_id` 等で集計するには **先に `Logs[].Message | fromjson` で展開** する必要がある:

```bash
# 直近 1 時間分の logs を local にダウンロード
# ※ date -d は GNU date (Linux) 専用。macOS (BSD date) では `date -u -v-1H +%Y%m%dT%H` を使う
wrangler r2 object list rshogi-csa-logs-prod --prefix "$(date -u -d '1 hour ago' +%Y%m%dT%H)" --remote 2>&1 | head -20
wrangler r2 object get rshogi-csa-logs-prod <object_key> --file /tmp/logs.ndjson.gz --remote
gunzip -k /tmp/logs.ndjson.gz   # /tmp/logs.ndjson が展開される

# 1 行 1 invocation wrapper → Logs[].Message を展開して 1 行 1 structured_log にフラット化
jq -c '.Logs[]?.Message? | select(type == "string") | fromjson? // empty' /tmp/logs.ndjson > /tmp/structured.ndjson
# (Message が JSON 形式で出ていない通常 console 出力は select で除外)

# event 別集計
jq -s 'group_by(.event) | map({event: .[0].event, count: length}) | sort_by(-.count)' /tmp/structured.ndjson

# 特定 game_id の全 log を時系列順に
jq -s 'sort_by(.ts_ms) | map(select(.game_id == "<game_id>"))' /tmp/structured.ndjson
```

### 7.2 NotificationPolicy alert を追加

Paid plan 移行と同時 or 別 PR で `failing_logpush_job_disabled_alert` を再 declare:

```bash
pulumi config set notificationsEnabled true
pulumi up
# → NotificationPolicy logpushFailureAlert が create され、Logpush 失敗時に Slack 通知が飛ぶ
```

`infra/pulumi/index.ts` の現コードは `alertWebhook && logpushJob` の両方が存在する時のみ NotificationPolicy を declare する条件分岐があり、§7.1.2 で logpushJob が active 化されると自動的に NotificationPolicy 候補に乗る。

### 7.3 wrangler.toml の `logpush = true` を有効化

`wrangler.{production,staging}.toml` で `logpush = true` 行が **コメントアウトされている** はず ([PR #704 post-merge fixup](https://github.com/SH11235/rshogi/pull/704) で対応、Free plan は `logpush` フラグを deploy gate で reject する `code 10023` の事故を経て訂正)。Paid 移行後は両 toml の該当行のコメントを外して `logpush = true` を有効化:

```toml
# 修正前 (Free plan)
# logpush = true

# 修正後 (Paid plan)
logpush = true
```

両 toml を変更 → 通常の deploy 経路 (CI `deploy-workers.yml` または手動 `wrangler deploy`) で反映。

> **教訓**: `wrangler.toml` 上の `logpush = true` は **Free plan で no-op ではなく deploy gate** (Cloudflare API `/workers/scripts/{name}` PUT request が plan check で reject)。Paid plan に upgrade する **前** に有効化してはいけない。逆に upgrade 直後 (Workers Logpush quota が allocate された) には有効化を忘れない。

### 7.4 production への展開

staging で `pulumi up` + R2 archive 確認 + alert test まで動作確認できたら、`pulumi stack select production` に切り替えて §7.1 〜 §7.2 を繰り返す。

## 8. 関連 Issue / PR / Doc

- [#625](https://github.com/SH11235/rshogi/issues/625): umbrella issue
- [#697](https://github.com/SH11235/rshogi/issues/697) / [PR #698](https://github.com/SH11235/rshogi/pull/698): Phase B Pulumi declare scaffold (merge 済)
- [#700](https://github.com/SH11235/rshogi/issues/700): 本 doc を Free plan 前提に書き直す PR (本 issue)
- [#691](https://github.com/SH11235/rshogi/pull/691): Phase A merge 済 (`structured_log!` macro 導入)
- [#671](https://github.com/SH11235/rshogi/pull/671): Phase C / [#630](https://github.com/SH11235/rshogi/issues/630) (synthetic monitoring) merge 済
- [#624](https://github.com/SH11235/rshogi/issues/624): R2 lifecycle / バックアップ — logs bucket も同 lifecycle 設計の対象 (Paid plan 移行時に再評価)
- [#628](https://github.com/SH11235/rshogi/issues/628): DO storage 喪失検知 alert (Free plan で実装可、§6 の方針で別 PR)
- [iac/docs/cloudflare-api-tokens.md](https://github.com/SH11235/iac/blob/main/docs/cloudflare-api-tokens.md): `pulumi-rshogi-iac` token の `Logs:Edit` / `Notifications:Edit` scope 詳細 (本 PR merge 後に Free plan 時点では Logs:Edit が unused である旨を別 PR で注記)
