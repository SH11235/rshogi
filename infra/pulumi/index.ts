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
// stack 切替: `pulumi stack select staging` / `pulumi stack select production`
// config: `accountId` (project namespace), `cloudflare:apiToken` (secret)

import * as pulumi from "@pulumi/pulumi";
import * as cloudflare from "@pulumi/cloudflare";

const config = new pulumi.Config();
const accountId = config.require("accountId");
const stack = pulumi.getStack();

// staging / production で同 shape の R2 bucket を 2 種ずつ持つ。
// bucket 名 / location / storageClass は wrangler 経由で作成された既存値を
// そのまま import している。
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
    ],
    production: [
        { pulumiName: "kifuProduction", bucketName: "rshogi-csa-kifu-prod" },
        {
            pulumiName: "floodgateHistoryProduction",
            bucketName: "rshogi-csa-floodgate-history-prod",
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
