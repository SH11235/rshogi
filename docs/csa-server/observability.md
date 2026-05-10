# rshogi-csa-server-workers Observability Runbook

[Issue #625](https://github.com/SH11235/rshogi/issues/625) で整備する 24/7 無人運用基盤の運用手順。本 doc は **Phase A 完了後の Phase B / C** の運用知識をまとめる。

- **Phase A** (✅ 完了 [PR #691](https://github.com/SH11235/rshogi/pull/691)): `structured_log!` macro 導入、全 `console_log!` を JSON 化
- **Phase B** (🚧 declare scaffold は [PR #697](https://github.com/SH11235/rshogi/issues/697) で merge 予定、bootstrap は user manual): Workers Logs → R2 archive + Cloudflare Notifications → Slack webhook
- **Phase C** (✅ 完了 [PR #671](https://github.com/SH11235/rshogi/pull/671) で [#630](https://github.com/SH11235/rshogi/issues/630) と統合): synthetic monitoring

## 1. アーキテクチャ概要

```
┌─────────────────────────────────────────────────────────────┐
│ Cloudflare Worker (rshogi-csa-server-workers)               │
│   ↓ structured_log!() macro (Phase A)                       │
│ Workers Logs (workers_trace_events dataset)                 │
└────────┬────────────────────────────────────────────────────┘
         │ Logpush (NDJSON、30 秒 batch)
         ↓
┌─────────────────────────────────────────────────────────────┐
│ R2 bucket: rshogi-csa-logs-{staging,prod}                   │
│   - 構造化ログの長期保存 + jq / grep / Cloudflare Workers 経由検索  │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ Cloudflare Notifications (account-level)                    │
│   - alertType: failing_logpush_job_disabled_alert           │
│     ↓ webhook (cf-webhook-auth header)                      │
│   - alertType: dos_attack_l7 (将来追加)                       │
└────────┬────────────────────────────────────────────────────┘
         │ POST request (Cloudflare-format JSON payload)
         ↓
┌─────────────────────────────────────────────────────────────┐
│ Slack incoming webhook (rshogi 警報専用 channel)             │
│   ※ Discord 切替時は translator Worker (別 PR で実装) 経由     │
└─────────────────────────────────────────────────────────────┘
```

## 2. 既存 IaC リソース

| Resource 種別 | 名前 (production) | 名前 (staging) | Pulumi 配置 |
|---|---|---|---|
| R2 bucket (logs archive) | `rshogi-csa-logs-prod` | `rshogi-csa-logs-staging` | `infra/pulumi/index.ts` (#697 で追加) |
| LogpushJob | `rshogi-csa-server-workers-production` | `rshogi-csa-server-workers-staging` | `infra/pulumi/index.ts` (#697 で追加) |
| NotificationPolicyWebhooks | `rshogi-production-alerts` (default) | `rshogi-staging-alerts` (default) | `infra/pulumi/index.ts` (#697 で追加) |
| NotificationPolicy (logpush 失敗) | `rshogi-production-logpush-failure` | `rshogi-staging-logpush-failure` | `infra/pulumi/index.ts` (#697 で追加) |

すべての resource は **Pulumi config secret が空の場合は declare されない** (`config.getSecret(...)` が undefined の時は resource 作成をスキップする条件分岐)。これは bootstrap 中の中途半端な state を構造的に防ぐ設計。

## 3. Bootstrap 手順 (Phase B 初回投入)

### 3.0 前提

- `infra/pulumi/index.ts` の Phase B scaffold は merge 済 ([PR #697](https://github.com/SH11235/rshogi/issues/697))
- Cloudflare Account ID (`d5d9818649d8722f73cd798c3b1ffb70`)
- Pulumi CLI ログイン済 (`pulumi whoami` で確認)

### 3.1 Cloudflare API token に scope 追加 (user manual)

`pulumi-rshogi-iac` token (`iac/docs/cloudflare-api-tokens.md` 参照) に以下 2 scope を追加:

- `Account: Logs: Write` (LogpushJob 作成に必須)
- `Account: Notifications: Write` (NotificationPolicy / Webhooks 作成に必須)

**手順** (token rotation):

1. CF Dashboard → My Profile → API Tokens → `pulumi-rshogi-iac` → Edit
2. Permissions に上記 2 行を追加 → Save
3. **token 値が変わらないので Pulumi config の更新は不要** (CF が token を再発行する操作ではないため)。ただし更新後に `pulumi preview --stack staging` で auth 通ることを確認する

代替案: 既存 token を revoke して新 token を発行 → `pulumi config set --secret cloudflare:apiToken --stack {staging,production}` (引数なしで対話 prompt 経由、§3.4 と同 pattern) で上書き。token 名前を変えたい場合 (例: `pulumi-rshogi-iac-v2`) はこちら。

### 3.2 R2 access key 発行 (Logpush destination 用、user manual)

LogpushJob は `r2://<bucket>/...?access-key-id=...&secret-access-key=...` 形式で R2 に書き込む。Pulumi が R2 bucket を declare する権限とは別に、**Logpush 自体が R2 に PUT する権限** が要る:

1. CF Dashboard → R2 → Manage R2 API Tokens → "Create API Token"
2. **Token name**: `logpush-rshogi-csa-logs` (任意)
3. **Permissions**: `Object Read & Write`
4. **Specify buckets**: `rshogi-csa-logs-staging` + `rshogi-csa-logs-prod` の 2 件のみ (Apply to specific buckets only)
5. **TTL**: 未設定 (年 1 review)
6. 発行後、画面に表示される `Access Key ID` と `Secret Access Key` を **その場でコピー** (二度と表示されない)

> **既存 wrangler 用 token / Pulumi 用 token を流用しない**: Logpush は R2 への書き込みを reservoir のように継続的に行うため、本 destination から credential が漏れた場合の影響範囲を logs bucket のみに閉じ込める設計とする (least privilege)。

### 3.3 Slack incoming webhook URL 取得 (user manual)

1. Slack workspace で **rshogi 警報専用 channel** を作成 (例: `#rshogi-alerts`、初期は user のみ参加)
2. https://api.slack.com/apps → "Create New App" → "From scratch" → name `rshogi-cloudflare-alerts` / 上記 workspace を選択
3. 左サイドバー "Incoming Webhooks" → "Activate Incoming Webhooks" を ON
4. "Add New Webhook to Workspace" → 上記 channel を選択 → Allow
5. 表示される `Webhook URL` (`https://hooks.slack.com/services/T.../B.../...`) を **その場でコピー** (再表示は可能だが秘匿管理)

### 3.4 Pulumi config 投入 (staging 先行)

> **重要**: secret 値を **shell 引数で渡さない**。`pulumi config set --secret KEY 'value'` の形式は `~/.bash_history` / `~/.zsh_history` に値が残り、後から `history` / `Ctrl-R` で見える状態になる。`--secret` フラグは Pulumi state 上の暗号化のみで、shell history 漏洩は防がない。
> 以下では `--secret` フラグのみ指定して **対話 prompt で stdin 入力** する形式 (Pulumi が echo を抑止する) と、**ファイル経由** で渡す形式の 2 通りを示す。

```bash
cd infra/pulumi
pulumi stack select staging

# 1. 平文で良い項目 (Cloudflare Notifications dashboard 表示名)
pulumi config set alertWebhookName 'rshogi-staging-alerts'

# 2. secret は --secret のみ指定 (値を引数に置かない) → Pulumi が
#    "Enter your value:" と prompt し、キー入力は echo されない
pulumi config set --secret alertWebhookUrl
# (Slack webhook URL を貼り付け → Enter)

# 3. translator Worker 経由にする場合の HMAC 検証用 random hex
#    (Slack 直結なら本ステップは省略可、Slack incoming webhook は
#    cf-webhook-auth header を無視する)
openssl rand -hex 32 | pulumi config set --secret alertWebhookSecret
# ↑ pipe 経由なら shell history には pulumi コマンドのみ残り値は残らない

# 4. Logpush destination URL (R2 access key + secret embedded)
#    URL は長いので /tmp の一時ファイル経由が安全 (作成直後に削除):
umask 077  # 作成ファイルを 600 で保護
cat > /tmp/logpush-destconf <<'DESTEOF'
r2://rshogi-csa-logs-staging/?account-id=d5d9818649d8722f73cd798c3b1ffb70&access-key-id=<ACCESS_KEY_ID>&secret-access-key=<SECRET_ACCESS_KEY>
DESTEOF
# ↑ <ACCESS_KEY_ID> / <SECRET_ACCESS_KEY> を §3.2 で発行した値で書き換えて保存
pulumi config set --secret logpushDestinationConf < /tmp/logpush-destconf
shred -u /tmp/logpush-destconf  # ファイルを上書き削除

# 5. enabled flag は平文 bool (true/false)。初期は両方 false で declare のみ
pulumi config set logpushEnabled false
pulumi config set notificationsEnabled false

# preview で 4 種 resource (R2 bucket / LogpushJob / NotificationPolicyWebhooks /
# NotificationPolicy) が create 予定であることを確認
pulumi preview

# 実 apply
pulumi up
```

> **shell history が既に汚染した場合**: `history -d <line_number>` で該当行を削除 + `history -c && history -w` で全消去。それでも `--secret` が掛かっているので Pulumi state 上は暗号化済 (Pulumi Cloud 側に平文は無い) だが、shell session 内 history file の上書きは別途必要。secret 値自体の rotation を強く推奨 (R2 access key / Slack webhook URL を再発行)。

`pulumi up` 直後の状態:
- ✅ R2 bucket 作成済 (新規 1 件、`protect: true`)
- ✅ NotificationPolicyWebhooks 作成済 (Cloudflare Notifications dashboard で確認可能)
- ✅ LogpushJob 作成済だが `enabled: false`
- ✅ NotificationPolicy 作成済だが `enabled: false`

### 3.5 Webhook 疎通確認

CF Dashboard → Notifications → Destinations → `rshogi-staging-alerts` を選択 → **"Send test notification"** クリック → Slack channel に test メッセージが届けば疎通 OK。

届かない場合のチェックポイント:
- Slack incoming webhook URL の typo
- Slack workspace の channel が消えている / Webhook App が disable
- (translator Worker 経由なら) Worker の URL / HMAC 検証の不一致

### 3.6 Logpush enable + 1 件目の archive 確認

```bash
pulumi config set logpushEnabled true
pulumi up
```

- 30 秒以内に Workers Log が R2 bucket に書き込まれ始める
- 確認: `wrangler r2 object list rshogi-csa-logs-staging` で 1 件以上 NDJSON object が出てくる
- Cloudflare Dashboard → Analytics & Logs → Logpush → `rshogi-csa-server-workers-staging` job の "Last 24h" success count を確認

### 3.7 NotificationPolicy enable

```bash
pulumi config set notificationsEnabled true
pulumi up
```

これで Logpush が連続失敗 (Cloudflare 側 retry 経過後 disable) した時に Slack 警報が飛ぶ状態になる。

### 3.8 production への展開

staging が一通り動作確認できたら、`pulumi stack select production` に切り替えて 3.4 〜 3.7 を繰り返す。本番 R2 bucket は `rshogi-csa-logs-prod`、Logpush job は `rshogi-csa-server-workers-production`。

production の `alertWebhookUrl` は staging と **同一 Slack channel に向けるか別 channel に分けるか** 運用判断 (Phase B 初期は同一 channel + tag で識別する low-tech 案を推奨)。

## 4. ログ検索 / 調査運用

### 4.1 ローカルから R2 archive を引く

```bash
# 直近 1 時間分の logs を local にダウンロード
wrangler r2 object list rshogi-csa-logs-prod --prefix "$(date -u -d '1 hour ago' +%Y%m%dT%H)" 2>&1 | head -20
wrangler r2 object get rshogi-csa-logs-prod <object_key> --file /tmp/logs.ndjson

# event 別集計
jq -s 'group_by(.event) | map({event: .[0].event, count: length}) | sort_by(-.count)' /tmp/logs.ndjson

# 特定 game_id の全 log を時系列順に
jq -s 'sort_by(.ts_ms) | map(select(.game_id == "<game_id>"))' /tmp/logs.ndjson
```

### 4.2 リアルタイム tail

```bash
# wrangler tail (in-memory、Logpush とは別経路、archive されない)
wrangler tail rshogi-csa-server-workers --format json | jq 'select(.event != null)'
```

## 5. Discord 切替方針 (将来)

Cloudflare Notifications は **Slack 形式 payload を送る前提**。Discord webhook は native 形式 (`{"content": ...}` or `{"embeds": [...]}`) を期待しており、Cloudflare が送る `{name, text, data, ts, policy_id, account_id}` 形式と互換性がない。

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
# (translator Worker URL を貼り付け → Enter)
pulumi up
```

`NotificationPolicyWebhooks` の `url` のみ差し替わり、`NotificationPolicy` 側は変更不要。HMAC 検証する場合は translator Worker 内で `cf-webhook-auth` header と `alertWebhookSecret` を比較する。

## 6. 関連 Issue / PR / Doc

- [#625](https://github.com/SH11235/rshogi/issues/625): umbrella issue
- [#697](https://github.com/SH11235/rshogi/issues/697): 本 PR (Phase B Pulumi declare scaffold)
- [#691](https://github.com/SH11235/rshogi/pull/691): Phase A merge 済 (`structured_log!` macro 導入)
- [#671](https://github.com/SH11235/rshogi/pull/671): Phase C / [#630](https://github.com/SH11235/rshogi/issues/630) (synthetic monitoring) merge 済
- [#624](https://github.com/SH11235/rshogi/issues/624): R2 lifecycle / バックアップ — logs bucket も同 lifecycle 設計の対象 (90 日 retention 等)
- [#628](https://github.com/SH11235/rshogi/issues/628): DO storage 喪失検知 alert を本 PR の NotificationPolicy 上に追加予定 (別 PR)
- [iac/docs/cloudflare-api-tokens.md](https://github.com/SH11235/iac/blob/main/docs/cloudflare-api-tokens.md): `pulumi-rshogi-iac` token の Logs:Write + Notifications:Write 追加 rotation 記録は本 PR merge 後の別 PR
