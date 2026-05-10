// rshogi-csa-server-workers の Cloudflare resources を Pulumi で IaC 管理する。
//
// Phase 1 (issue #676) スコープ:
// - R2 buckets のみ宣言する。
// - Worker script (rshogi-csa-server-workers / -staging) と DO bindings /
//   migrations / vars / secrets / cron triggers は **wrangler.{staging,production}.toml
//   による管理を継続** する。Pulumi @pulumi/cloudflare v6 では WASM Worker
//   (Rust → WASM build) の content を marshal する際に UTF-8 エラーで import
//   が成立せず、cron triggers も provider bug で schedules が空配列で state
//   投入される事象があるため、Phase 1 のスコープ外とする。
// - Phase 3 (#675 umbrella) の WAF rule は Worker 名を string で参照するだけ
//   で良いため、Worker 自体を Pulumi 管理する必要は当面ない。
//
// Phase B (#625 / issue #697) で追加 (2026-05-10):
// - Logs archive 用 R2 bucket (rshogi-csa-logs-{staging,prod}) を追加宣言
// - cloudflare.LogpushJob を declare して Workers Logs を NDJSON で R2 archive に流す
// - cloudflare.NotificationPolicyWebhooks を declare して webhook 配信先を登録
// - cloudflare.NotificationPolicy を declare して 最小有効 alert (Logpush 失敗) を設定
// - Slack webhook URL / R2 access key / Logpush enabled flag は **Pulumi config
//   secret 経由** で投入する。本ファイルは declare scaffold のみで、bootstrap 手順
//   (config secret 投入 + token rotation) は user manual。
//   詳細は docs/csa-server/observability.md §3 を参照。
//
// stack 切替: `pulumi stack select staging` / `pulumi stack select production`
// config: `accountId` (project namespace), `cloudflare:apiToken` (secret)
// Phase B 追加 config (project namespace、全て secret):
//   - `alertWebhookName`: Webhook 配信先の表示名 (Cloudflare Notifications dashboard 用)
//   - `alertWebhookUrl`: Slack incoming webhook URL (Discord 切替時は translator Worker URL)
//   - `alertWebhookSecret`: HMAC verification secret (cf-webhook-auth header 検証)
//   - `logpushDestinationConf`: Logpush destination URL (`r2://<bucket>/<path>?...`)
//   - `logpushEnabled`: bool (default false)、bootstrap 完了後に true へ flip
//   - `notificationsEnabled`: bool (default false)、webhook 配信先疎通確認後に true へ flip

import * as pulumi from "@pulumi/pulumi";
import * as cloudflare from "@pulumi/cloudflare";

const config = new pulumi.Config();
const accountId = config.require("accountId");
const stack = pulumi.getStack();

// staging / production で同 shape の R2 bucket を 3 種ずつ持つ。
// bucket 名 / location / storageClass は wrangler 経由で作成された既存値を
// そのまま import している (logs bucket のみ Phase B で新規作成、import 不要)。
//
// 命名規約 (Cloudflare): 小文字英数字 + ハイフン、3〜63 文字、先頭末尾は英数字。
//
// jurisdiction "default" / location "APAC" / storageClass "Standard" は
// 既存 bucket を import した時点の値で、CSA Worker (`game_room.rs` 終局経路)
// から PUT される頻度・サイズに対して特別な調整は不要 (Cloudflare 既定で十分)。

type SupportedStack = "staging" | "production";

interface R2BucketSpec {
    pulumiName: string;
    bucketName: string;
}

const bucketSpecs: Record<SupportedStack, R2BucketSpec[]> = {
    staging: [
        // GameRoom DO が終局時に CSA V2 棋譜を書き出す先。
        { pulumiName: "kifuStaging", bucketName: "rshogi-csa-kifu-staging" },
        // 1 対局 = 1 オブジェクト形式の Floodgate 履歴の書き出し先。
        {
            pulumiName: "floodgateHistoryStaging",
            bucketName: "rshogi-csa-floodgate-history-staging",
        },
        // Phase B (#625): Workers Logs (LogpushJob) の NDJSON archive 先。
        // 本 bucket は Cloudflare Logpush から書き込まれる (Worker からは触らない)。
        { pulumiName: "logsStaging", bucketName: "rshogi-csa-logs-staging" },
    ],
    production: [
        { pulumiName: "kifuProduction", bucketName: "rshogi-csa-kifu-prod" },
        {
            pulumiName: "floodgateHistoryProduction",
            bucketName: "rshogi-csa-floodgate-history-prod",
        },
        // Phase B (#625): production 側 logs archive。
        {
            pulumiName: "logsProduction",
            bucketName: "rshogi-csa-logs-prod",
        },
    ],
};

function isSupportedStack(s: string): s is SupportedStack {
    return s === "staging" || s === "production";
}

if (!isSupportedStack(stack)) {
    throw new Error(
        `unsupported stack: ${stack}. Expected one of: staging, production`,
    );
}

const stackSpecs = bucketSpecs[stack];

export const buckets = stackSpecs.map(
    (spec) =>
        new cloudflare.R2Bucket(
            spec.pulumiName,
            {
                accountId,
                jurisdiction: "default",
                location: "APAC",
                name: spec.bucketName,
                storageClass: "Standard",
            },
            { protect: true },
        ),
);

// ---------------------------------------------------------------------------
// Phase B (#625): observability scaffold
// ---------------------------------------------------------------------------
//
// 全リソースは「config secret が **空でない値で** 設定済の場合のみ実体化」する。
// 未設定 or 空文字列の場合は `pulumi up` で作成されない (ResourceOptions の
// `if-then-else` 相当を JS の条件分岐で表現)。これにより:
//
// - 初回 `pulumi up` (config 未投入) はノーオペで通る (declare 追加だけで CI green)
// - bootstrap 中 (config 投入後) に webhook → policy の順で安全に enable していける
// - 障害時に config を空にして `pulumi up` で revoke / detach が可能 (rollback path)
//
// 「declare があるが値が空」状態を Pulumi state 上で持たないことで、
// 中途半端な resource (空 URL の webhook 等) を Cloudflare に投入してしまう
// 事故を構造的に防ぐ。
//
// **空文字列ガード**: Pulumi config の `getSecret` は config 値が "" でも
// truthy な `Output<string>` を返すため、`if (output)` チェックは値の有無を
// 判別できない。`config.get(key)` (sync raw read) で synchronously 確認してから
// `requireSecret` で secret-tagged Output を取り直す。

function readOptionalSecret(key: string): pulumi.Output<string> | undefined {
    const raw = config.get(key);
    if (raw === undefined || raw.length === 0) {
        return undefined;
    }
    // raw が non-empty なら secret として再読み (Pulumi state 上で secret tag を保つ)
    return config.requireSecret(key);
}

const alertWebhookUrl = readOptionalSecret("alertWebhookUrl");
const alertWebhookSecret = readOptionalSecret("alertWebhookSecret");
const alertWebhookName =
    config.get("alertWebhookName") ?? `rshogi-${stack}-alerts`;

// ---- NotificationPolicyWebhooks (alert 配信先登録) -----------------------
//
// `cf-webhook-auth` header に `alertWebhookSecret` の値が乗る。受け側
// (Slack incoming webhook / Discord webhook / translator Worker) で HMAC 検証
// する場合に利用する。Slack incoming webhook は cf-webhook-auth を無視する
// ので疎通確認時は secret なしでも動く (推奨は HMAC 検証経路を持つ
// translator Worker 経由で受けて secret を検証すること)。

export const alertWebhook = alertWebhookUrl
    ? new cloudflare.NotificationPolicyWebhooks(`alertWebhook-${stack}`, {
          accountId,
          name: alertWebhookName,
          url: alertWebhookUrl,
          ...(alertWebhookSecret ? { secret: alertWebhookSecret } : {}),
      })
    : undefined;

// ---- LogpushJob (Workers Logs → R2 archive) -----------------------------
//
// `dataset: workers_trace_events` は Cloudflare Workers の請求対象外 dataset
// (Logpush は Workers Paid plan で利用可、Free plan は対象外 dataset 限定)。
// rshogi-csa-server-workers は Paid plan で運用しているため利用可能。
//
// `destinationConf` は R2 への signed URL 形式:
//   `r2://<bucket>/<path>?account-id=<account>&access-key-id=<r2_key>&secret-access-key=<r2_secret>`
// R2 access key は Cloudflare Dashboard → R2 → Manage R2 API Tokens で発行。
// 既存の wrangler 用 token とは別の R2-only token を発行することを推奨
// (least privilege 原則、本 destination から漏れた場合の影響範囲を logs bucket
// のみに閉じ込める)。詳細は docs/csa-server/observability.md §3.2 を参照。
//
// `outputOptions.outputType: "ndjson"` で 1 行 1 JSON で archive (構造化ログの
// 性質を保ったまま jq / grep で検索可能)。`timestampFormat: "rfc3339"` で
// ts_ms との整合 (structured_log! macro が出す ts_ms は milliseconds、Logpush
// の追加 timestamp は rfc3339)。

const logpushDestinationConf = readOptionalSecret("logpushDestinationConf");
const logpushEnabled = config.getBoolean("logpushEnabled") ?? false;

// Worker script 名 (LogpushJob の name + filter で参照される single source of truth)。
// `wrangler.{stack}.toml` の `name` フィールドと一致させる契約。
// 不一致だと `logpush = true` 設定済 Worker のログが filter で除外され、R2 archive が空になる。
const workerScriptName =
    stack === "staging"
        ? "rshogi-csa-server-workers-staging"
        : "rshogi-csa-server-workers";

export const logpushJob = logpushDestinationConf
    ? new cloudflare.LogpushJob(`workersLogpush-${stack}`, {
          accountId,
          name: workerScriptName,
          dataset: "workers_trace_events",
          destinationConf: logpushDestinationConf,
          enabled: logpushEnabled,
          // 30 秒間隔 / 5 MiB / 1000 records のいずれかで batch flush。
          // 構造化ログ JSON 1 行 ≈ 200〜500 byte 想定で、平常時の log 量
          // (handle_line / lobby / room_join 各 ~10/sec ピーク見積) でも
          // 30 秒以内に flush される size に到達せず、interval が支配的になる。
          maxUploadIntervalSeconds: 30,
          // 5 MiB (= 5 * 1024 * 1024)。Cloudflare Logpush の standard デフォルトに揃える
          // (Codex review: `5_000_000` は 5 MB で off-by-one、推奨は MiB 換算)。
          maxUploadBytes: 5_242_880,
          maxUploadRecords: 1000,
          // `workers_trace_events` dataset から R2 archive に書き出す field を明示列挙する。
          // 列挙しないと Cloudflare がデフォルトで全 field を embed してオブジェクトサイズが
          // 膨らむ + 構造化ログ (`Logs[].Message` に含む) の取り出しコストが上がる。
          // 詳細は https://developers.cloudflare.com/logs/reference/log-fields/account/workers_trace_events/
          outputOptions: {
              outputType: "ndjson",
              timestampFormat: "rfc3339",
              fieldNames: [
                  "EventTimestampMs",
                  "ScriptName",
                  "ScriptVersion",
                  "Outcome",
                  "EventType",
                  "Logs",
                  "Exceptions",
                  "DispatchNamespace",
                  "RequestHeaders",
                  "ResponseHeaders",
                  "Event",
                  "Diagnostics",
              ],
          },
          // filter で本 Worker script のログのみに絞る (account 内の他 Worker
          // ログを巻き込まない)。`ScriptName` の正確な値は wrangler.toml の
          // [env].name に一致する (`workerScriptName` 変数を single source として使用)。
          filter: JSON.stringify({
              where: {
                  key: "ScriptName",
                  operator: "eq",
                  value: workerScriptName,
              },
          }),
      })
    : undefined;

// ---- NotificationPolicy (alert ルール) -----------------------------------
//
// Phase B 初版では「Logpush 自体が止まったら alert」の 1 件のみ宣言する。
// これは observability の根幹 (logs が止まれば alert もできなくなる) を
// 守る fail-safe。追加 alert (5xx burst / DO error 等) は別 PR で
// 同パターンで extend する (本ファイルに `additionalNotificationPolicies` 配列を
// 足す形で増やせる)。
//
// alertType の選定:
// - `failing_logpush_job_disabled_alert`: Logpush job が連続失敗で
//   Cloudflare 側に自動 disable された時に発火 (=「ログが取れなくなった」検知)
// - 5xx は Worker の場合 zone 経由ではなく Logpush データを external 監視で
//   集計する方が確実 (Cloudflare Notifications の `http_alert_origin_error` は
//   zone scope で Workers behind custom domain で限定的に動く)。本 PR では
//   declare せず別 PR で `dos_attack_l7` 等と一緒に評価する。

const notificationsEnabled =
    config.getBoolean("notificationsEnabled") ?? false;

export const logpushFailureAlert =
    alertWebhook && logpushJob
        ? new cloudflare.NotificationPolicy(`logpushFailureAlert-${stack}`, {
              accountId,
              name: `rshogi-${stack}-logpush-failure`,
              alertType: "failing_logpush_job_disabled_alert",
              enabled: notificationsEnabled,
              description:
                  "rshogi-csa-server-workers Logpush が連続失敗で disable された場合に発火。observability 根幹の fail-safe として #625 Phase B で declare。",
              // `alertInterval` は本 alertType (failing_logpush_job_disabled_alert) では
              // Cloudflare Notifications API が無視する (single-shot alert として処理)。
              // 5xx burst 等の continuous alert を別 PR で追加する際は `alertInterval: "30m"`
              // を再導入する判断とする。
              mechanisms: {
                  webhooks: [{ id: alertWebhook.id }],
              },
          })
        : undefined;
