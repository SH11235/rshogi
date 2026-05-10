# rshogi-csa-server-workers IaC (Pulumi) Runbook

`infra/pulumi/` に置いた Pulumi project で Cloudflare 上の rshogi-csa-server-workers
関連リソースを **段階的に** IaC 管理する運用手順。

[#675 umbrella](https://github.com/SH11235/rshogi/issues/675) の Phase 1
([#676](https://github.com/SH11235/rshogi/issues/676)) 着地時点では **R2 buckets 4 件のみ Pulumi 管理** で、Worker
script / DO bindings / migrations / vars / secrets / cron triggers は引き続き
`crates/rshogi-csa-server-workers/wrangler.{staging,production}.toml` で
管理する (理由は §3 参照)。

## 1. アーキテクチャ概要

```
┌──────────────────── Pulumi Cloud (sh11235 個人 org) ────────────────────┐
│ project: rshogi-csa-server-workers                                    │
│   stack: staging     ─── R2: rshogi-csa-kifu-staging                  │
│                     ─── R2: rshogi-csa-floodgate-history-staging      │
│   stack: production  ─── R2: rshogi-csa-kifu-prod                     │
│                     ─── R2: rshogi-csa-floodgate-history-prod         │
└───────────────────────────────────────────────────────────────────────┘
            ▲                                          ▲
            │ pulumi up (R2 のみ)                       │ wrangler deploy
            │                                          │ (Worker / DO / vars / secrets / cron)
            │                                          │
   ┌─────────────────────┐                  ┌─────────────────────────────┐
   │ infra/pulumi/       │                  │ crates/rshogi-csa-server-   │
   │   index.ts          │                  │   workers/                  │
   │   Pulumi.<stack>.yaml │                │     wrangler.<stack>.toml   │
   └─────────────────────┘                  └─────────────────────────────┘
```

「**R2 = Pulumi**, **Worker 関連 = wrangler**」の責務分離を Phase 1 では維持する。
Phase 2 以降で WAF / Cloudflare Access / DNS 等の zone-level resource を
Pulumi 配下に追加していく ([#675 umbrella](https://github.com/SH11235/rshogi/issues/675) Phase 2/3 参照)。

## 2. 初回セットアップ (新規 operator 向け)

### 2.1 必要なもの

| 区分 | 要件 |
|---|---|
| Pulumi CLI | v3.237.0+ (`pulumi version` で確認) |
| Pulumi Cloud アカウント | Individual tier で十分 (個人プロジェクト無料) |
| Cloudflare API token | §2.3 の scope を満たす token |
| Cloudflare Account ID | Cloudflare dashboard 右サイドバーから取得 |
| Node | v24.15.0 (`.node-version`) |
| pnpm | v10.33.2 (`packageManager` で pin、Vite+ 経由で `vp install` 推奨) |

### 2.2 Pulumi CLI install + login

```bash
# install (推奨は公式インストーラ、asdf / mise 等でも可)
curl -fsSL https://get.pulumi.com | sh

# Pulumi Cloud にログイン (ブラウザで OAuth)
pulumi login
# → Logged in to pulumi.com as <your-username>

pulumi whoami   # 自分の personal org 名が表示されればOK
```

### 2.3 Cloudflare API token 発行

https://dash.cloudflare.com/profile/api-tokens → "Create Token" → "Create Custom Token"

Phase 1 では Pulumi が R2 buckets のみ管理するため、**最小権限は R2 Storage Edit + Account Settings Read の 2 行**。Worker 関連 (`Workers Scripts: Edit` 等) は Phase 1 では不要 — 将来 Worker を Pulumi 配下に入れる段階 (umbrella [#675](https://github.com/SH11235/rshogi/issues/675) Phase 4 想定) で追加する。

| 項目 | 値 |
|---|---|
| Token name | `pulumi-rshogi-iac` (任意、用途識別用) |
| Permissions | Account → Workers R2 Storage: Edit |
|  | Account → Account Settings: Read |
| Account Resources | Include → Specific account → 自分の個人 account |
| Zone Resources | 設定不要 (Custom Domain 未使用) |
| TTL | 未設定 |

→ token 文字列をコピー (1 回しか表示されない)。

> **既存の wrangler 用 `CLOUDFLARE_API_TOKEN` を流用しない**:
> Pulumi は wrangler と異なるリソース集合 (R2 Storage) を独占管理する。
> 同じ token を使うと audit log で wrangler / pulumi 由来を識別できず、
> rotation 時の影響範囲も切り分けにくくなる。独立 token を推奨。

> **token 権限拡張時の rotation 契約**: 将来 Worker 等を Pulumi 配下に追加
> する PR では、本 §2.3 の Permissions 表を更新 + 既存 token を新 scope で
> 再発行 + Pulumi.{stack}.yaml の `cloudflare:apiToken` を再投入の **3 点
> セット** で扱う。表だけ広げて token を rotation し忘れると最小権限原則が
> 崩れるので、PR review で 3 点同期を確認する運用とする。

### 2.4 Pulumi project の deps install

```bash
cd infra/pulumi
vp install   # 推奨: Vite+ 経由 (pnpm のバージョン pin と Node 版を一括管理)
```

**`vp` 未導入環境** (CI / 新規 contributor 環境等) では素の pnpm を使う:

```bash
cd infra/pulumi
corepack enable                 # Node 同梱の corepack で pnpm を有効化
pnpm install --frozen-lockfile  # packageManager フィールドの pnpm@x.y.z が解決される
```

Vite+ (`vp`) は本リポの他ディレクトリ (`crates/rshogi-csa-server-workers/`、`ramu-shogi/`、`nnue-lab/` 等) で採用している managed mode の Node toolchain。https://viteplus.dev/ を参照。
普段 vp を使う運用者なら `vp install` の方が Node version も自動解決されて楽。

### 2.5 stack 切替 + config 投入

stack は `staging` と `production` の 2 つ。各 stack で:

```bash
pulumi stack select staging        # or production
pulumi config set --secret cloudflare:apiToken '<貼り付け>'
pulumi config set accountId       '<account_id>'   # secret ではない (URL に出る情報)
```

> **config namespace に注意**: `@pulumi/cloudflare` v6 では `accountId` は
> provider-level config を持たず、各 resource の引数として受け取る。
> したがって `cloudflare:accountId` ではなく project namespace
> (`rshogi-csa-server-workers:accountId`、CLI では prefix なしで `accountId`)
> に置く。誤って `cloudflare:accountId` で set すると
> "not a valid configuration key for the cloudflare provider" エラーになる。

## 3. Phase 1 で Worker を Pulumi 配下に入れていない理由

`@pulumi/cloudflare` v6 (現行) には WASM Worker (Rust → WASM build) との
互換性問題がある:

- **`cloudflare.WorkersScript` resource**: `pulumi import` 時に Worker の
  script content を string field として marshal しようとして
  `grpc: error while marshaling: string field contains invalid UTF-8` で失敗。
  本リポは `worker-build --release` で生成した WASM binary が content 本体
  なので、provider 側で binary-safe な扱いが必要だが現状未対応。
- **`cloudflare.WorkersCronTrigger` resource**: `pulumi import` 時に
  `schedules` を Cloudflare API から正しく読み出せず空配列 `[]` で state に
  入る (provider bug)。state と Cloudflare 実体が乖離した silent drift に
  なるため、wrangler.toml の `[triggers] crons = [...]` 経由管理を継続。
- **DO bindings / migrations / vars / secrets**: 上記 `WorkersScript` の
  内部フィールドとして管理されるため、Worker が import できない以上
  Pulumi 側で扱えない。

`WorkersScript` の docstring 上は beta `cloudflare.Worker` +
`cloudflare.WorkerVersion` + `cloudflare.WorkersDeployment` への移行を
推奨しているが、`WorkerVersion` で content を declare すると Pulumi 経由で
content upload が必須化され、wrangler-based deploy との二重管理が発生する。
これは [#676](https://github.com/SH11235/rshogi/issues/676) スコープ内では避けたいので、Phase 1 では Worker 管理を
wrangler に残す決定とした (umbrella [#675](https://github.com/SH11235/rshogi/issues/675) Phase 4 で再検討)。

## 4. 通常運用フロー

> **対象読者**: 既に bootstrap 済み (Pulumi Cloud project + 両 stack + R2 4 件 import 済) の状態を引き継ぐ運用者。
> 通常運用では **新規 stack を作らない** / **`pulumi up` で R2 bucket を新規作成しない** のが既定 (既存 bucket は `protect: true` で守られているため誤 destroy も block される)。
> bootstrap が必要な状況 (新規アカウント / 旧 state 喪失 / 別環境追加) は §4.4 参照。

### 4.1 R2 bucket 設定の変更 (lifecycle, CORS など追加する場合)

```bash
cd infra/pulumi
pulumi stack select staging
# index.ts を編集 (例: cloudflare.R2BucketCors を追加)
pulumi preview              # 差分確認、staging で先に試す
pulumi up                   # 適用
# staging で観察 → production へ展開
pulumi stack select production
pulumi preview
pulumi up
```

### 4.2 wrangler 経由 deploy との関係

- Worker / DO / vars / cron / secrets の変更は今まで通り
  `wrangler.{staging,production}.toml` を編集して `wrangler deploy` (CI 経由
  `.github/workflows/deploy-workers.yml` で自動化済み) で反映する
- R2 binding の `bucket_name` は wrangler.toml にも書かれているが、bucket
  自体の作成/削除/設定は Pulumi 側が source of truth。bucket 名が一致して
  いれば binding は wrangler の意図通り張られる
- bucket 名を変更する場合は **Pulumi 側で bucket を新名で作成 → wrangler.toml
  の bucket_name を更新 → wrangler deploy → Pulumi 側で旧 bucket を destroy**
  の順 (worker 側 binding が消える前に bucket を消すと一時的にデータ書き込み
  失敗するため)

### 4.3 secret 値の追加 / rotation

Phase 1 時点では Worker secret (`ADMIN_API_TOKEN` など) は **wrangler 経由のみ**
で管理する:

```bash
cd crates/rshogi-csa-server-workers
vp exec wrangler secret put ADMIN_API_TOKEN --config wrangler.staging.toml
vp exec wrangler secret put ADMIN_API_TOKEN --config wrangler.production.toml
```

詳細手順は `docs/csa-server/admin_auth.md` を参照。
Phase 2 で Pulumi config / Pulumi ESC への移行を検討する。

### 4.4 bootstrap (新規 stack / 別環境 / state 復旧)

**通常運用では使わない**。以下の状況でのみ実行:

- 新規 Cloudflare アカウントに本 Pulumi project を移植する
- 既存 Pulumi Cloud stack を消してしまい再作成する必要がある
- staging / production 以外の新環境 (例: pr-preview 環境) を追加する

手順:

```bash
cd infra/pulumi
pulumi stack init <new-stack-name>
pulumi config set --secret cloudflare:apiToken '<token>'
pulumi config set accountId '<account_id>'

# 既存 R2 buckets を import (Cloudflare 上に既に存在している場合)
# import ID 形式: <account_id>/<bucket_name>/<jurisdiction>
# jurisdiction は "default" / "eu" / "fedramp" のいずれか (新規発行時は "default")
pulumi import 'cloudflare:index/r2Bucket:R2Bucket' kifuStaging \
    "<account_id>/rshogi-csa-kifu-staging/default"
pulumi import 'cloudflare:index/r2Bucket:R2Bucket' floodgateHistoryStaging \
    "<account_id>/rshogi-csa-floodgate-history-staging/default"
# (production の場合は kifuProduction / floodgateHistoryProduction、bucket 名 -prod)

# index.ts の bucketSpecs に新環境の対応エントリを追加 + stack === "..." の switch 追加
# その後 pulumi preview で diff 0 を確認
pulumi preview
```

> **誤 `pulumi up` で bucket を新規作成しない**: index.ts に bucketSpecs エントリだけ
> 追加して import を忘れると、`pulumi up` は「Cloudflare 上に存在しない bucket を
> 作る」操作と解釈する。新名で発行されたり既存と衝突するとデータ消失リスクあり。
> 必ず import 完了 + `pulumi preview` diff 0 を確認してから `pulumi up` する。

## 5. 緊急ロールバック

### 5.1 R2 bucket の設定変更を取り消す

```bash
pulumi stack history --stack staging   # 履歴確認 (Pulumi Cloud Console でも見える)
# 直前の commit へ revert する場合
git revert <commit>
pulumi preview && pulumi up
```

### 5.2 R2 bucket そのものを誤って destroy しそうな PR が来た場合

R2 bucket は `protect: true` で守られているため、`pulumi destroy` や `pulumi up`
で resource が消える操作は failed する。意図して destroy する場合のみ
`pulumi state unprotect <urn>` → `pulumi destroy` を使う。

## 6. CI 連携 (Phase 1 では preview のみ)

`.github/workflows/pulumi-preview.yml` は **2 job 構成**:

- **`validate-pr` (`pull_request` trigger)**: secrets を一切使わない静的検証
  (TypeScript 型 check + pnpm install dry)。PR で変更された Pulumi code が
  `PULUMI_ACCESS_TOKEN` 経由で Cloudflare token を漏洩する経路を作らない
  ための分離。
- **`preview-staging` (`workflow_dispatch` only)**: 信頼済み運用者が手動で
  staging stack の `pulumi preview` を実行する経路。`PULUMI_ACCESS_TOKEN`
  を使用。

### 6.1 必要な secret / config (実 preview を動かす場合)

- **GitHub repo secret**: `PULUMI_ACCESS_TOKEN`
  - https://app.pulumi.com/account/tokens で Personal Access Token を発行
  - リポジトリ Settings → Secrets and variables → Actions に登録
- **Pulumi Cloud stack config** (`Pulumi.staging.yaml` / `Pulumi.production.yaml`):
  - `cloudflare:apiToken` (encrypted secret) — §2.5 で投入済
  - `rshogi-csa-server-workers:accountId` — §2.5 で投入済

両方揃っていないと `preview-staging` job は途中で失敗する。`PULUMI_ACCESS_TOKEN` だけ未設定の場合は warning + skip で job 自体は green に抜ける (secrets 設定後の最初の dispatch で実 preview が走る)。stack config 不足の場合は `pulumi preview` 内部で error 終了するため job は red になる。

### 6.2 自動 `pulumi up` を Phase 1 で行わない理由

現行 wrangler 配線が動いている間に Pulumi 側 deploy 経路も自動化すると競合 / 想定外 deploy のリスクがある。Phase 2 以降で CI 自動 deploy を慎重に統合する。

## 7. トラブルシューティング

### 7.1 `cloudflare:accountId is not a valid configuration key`

§2.5 「config namespace に注意」を参照。`pulumi config rm cloudflare:accountId`
→ `pulumi config set accountId <id>` で project namespace に置き直す。

### 7.2 `string field contains invalid UTF-8` で WorkersScript import が失敗

§3 参照。Phase 1 では Worker を Pulumi 管理しないので発生しない経路だが、
将来 Worker を追加しようとした場合は Pulumi v6 provider 側の WASM 対応待ち。

### 7.3 Pulumi.<stack>.yaml に encrypted secret が出るが commit してよいか

OK。Pulumi Cloud (SaaS backend) を使っている前提で、secret は service-side
key で encrypted されている (`secure: AAA...` の形式)。token 値そのものは
含まない。

Self-managed backend (R2 / S3 等) を使う場合は `PULUMI_CONFIG_PASSPHRASE`
依存になり commit 可否が変わるので、Phase 2 で backend 移行する場合は
本セクションを更新する。

## 9. Secret 管理 (Phase 2-D 以降)

[#690](https://github.com/SH11235/rshogi/issues/690) で **Worker secret の値だけ
Pulumi ESC を single source of truth に集約** した。Worker script / DO / vars /
cron triggers の管理境界 (§3) は変えず、secret 値のみ「ESC に置き、
`secret-sync.yml` で wrangler 経由 Cloudflare に流す」運用に移行する。

§4.3 は Phase 1 時点での「wrangler 直叩きで `secret put`」手順を記録した
historical 節として残す (旧手順を急遽踏みたい emergency rotation 用に温存)。
通常 rotation は本 §9 の手順で行うこと。

### 9.1 アーキテクチャ (採用案 B)

```
┌─ Pulumi ESC (sh11235 org) ──────────────────────────────────┐
│ env: sh11235/rshogi-csa-server-workers-staging              │
│   values.workerSecrets.ADMIN_API_TOKEN  (encrypted)          │
│ env: sh11235/rshogi-csa-server-workers-production           │
│   values.workerSecrets.ADMIN_API_TOKEN  (encrypted)          │
└──────────────────────────────────┬──────────────────────────┘
                                   │ (1) esc env open --format json
                                   ▼
                        ┌──────────────────────────┐
                        │ secret-sync.yml          │
                        │ (workflow_dispatch only) │
                        └──────────────┬───────────┘
                                       │ (2) wrangler secret bulk
                                       ▼
                  ┌──────────────────────────────────────┐
                  │ Cloudflare Worker secret store       │
                  │   ADMIN_API_TOKEN (per env)          │
                  └──────────────────────────────────────┘
```

候補比較:

| 案 | 概要 | 採否 | 理由 |
|---|---|---|---|
| A | Pulumi `WorkersScript` の `secret_text` binding で declarative 管理 | 不採用 | `@pulumi/cloudflare` v6 の WASM Worker marshal error が未解決 (§3 参照) |
| B | ESC + `workflow_dispatch` + wrangler kick | **採用** | Worker script を Pulumi 配下に入れずに secret 値だけ集約できる |
| C | 台帳のみ管理 (人手で wrangler 叩く) | 不採用 | 自動化目的を満たさない |

### 9.2 ESC environment 構造の規約

ESC environment 名は **per-repo per-env**:

- `sh11235/rshogi-csa-server-workers-staging`
- `sh11235/rshogi-csa-server-workers-production`

YAML 構造の規約:

```yaml
values:
  workerSecrets:
    ADMIN_API_TOKEN:
      fn::secret:
        ciphertext: <encrypted ciphertext>
    # 将来 Worker secret を追加するときは workerSecrets 配下にキーを足す
```

`values.workerSecrets` 配下のキー名がそのまま `wrangler secret bulk` に渡され、
Worker code 側 `env.var("KEY")` で参照される名前と一致する必要がある。
`workerSecrets` キーが空 / 不在のまま `secret-sync.yml` を起動すると
fail-closed で abort する (空 push で既存 secret を消去 / 上書きしない既定)。

`esc env open --format json` は decrypt 済の **flat** JSON を吐く (top-level に
`workerSecrets` object が現れる、`values` 等のネストは出ない)。デバッグ時の参考:

```bash
$ esc env open sh11235/rshogi-csa-server-workers-staging --format json
{
  "workerSecrets": {
    "ADMIN_API_TOKEN": "<plaintext value>"
  }
}
```

`secret-sync.yml` は `.workerSecrets` を `jq` で抽出してそのまま `wrangler secret
bulk` に渡す。`workerSecrets` が `values` ネスト下にある等の構造ミスマッチを
踏んだ場合は workflow が fail-closed で abort し、エラー log に top-level keys
を出すので構造を確認できる (値は出さない)。

### 9.3 通常の secret 追加 / rotation 手順

```bash
# 1. ESC env を編集して新値を投入 (standalone esc CLI 経由)
esc env edit sh11235/rshogi-csa-server-workers-staging
#   → エディタが開くので values.workerSecrets.<KEY> を fn::secret で追加 / 更新
#   (詳細は https://www.pulumi.com/docs/esc/cli/commands/esc_env_edit/ 参照)
#   ※ Pulumi CLI を入れているなら `pulumi env edit ...` でも同等

# 2. ESC 単体で値を確認 (workflow を流さずに dry-run したい場合)
esc env open sh11235/rshogi-csa-server-workers-staging --format json | \
    jq '.workerSecrets | keys'
#   ※ workflow と同じ standalone `esc` CLI を使う。Pulumi CLI 経由なら `pulumi env open ...`

# 3. workflow_dispatch で wrangler 同期を kick
gh workflow run secret-sync.yml --repo SH11235/rshogi -f environment=staging

# 4. workflow log で「Fetched N secret key(s)」「Successfully created secret for key: ...」
#    を確認
gh run watch --repo SH11235/rshogi   # or: gh run list --workflow secret-sync.yml --repo SH11235/rshogi

# 5. Cloudflare 側に反映されたか確認
cd crates/rshogi-csa-server-workers
npx wrangler secret list --config wrangler.staging.toml
#   → ADMIN_API_TOKEN: SECRET_TEXT が並んでいれば成功
```

production も同手順で `staging` を `production` に置換するだけ。

### 9.4 secret-sync.yml の責務範囲

- **やること**:
  - 指定 `environment` の ESC env を `esc env open --format json` で読み出す
  - `.workerSecrets` を flat JSON object として書き出し、`wrangler secret bulk`
    で 1 回の API 呼び出しで投入
  - `Step Summary` に同期対象の ESC env 名 / wrangler config 名 / 反映確認コマンドを残す
- **やらないこと**:
  - Worker script 本体の deploy (= `deploy-workers.yml` の責務)
  - ESC 側の値変更 (= 運用者が `esc env edit` で行う)
  - 同期失敗時の自動 rollback (= ESC 値を戻して再 dispatch する手動 rollback 運用)

### 9.5 必要 GitHub secret / 権限

| key | 用途 | 流用元 |
|---|---|---|
| `PULUMI_ACCESS_TOKEN` | ESC env 読み取り | `pulumi-preview.yml` で既設定 |
| `CLOUDFLARE_API_TOKEN` | `wrangler secret bulk` の Cloudflare API 呼び出し | `deploy-workers.yml` で既設定 (`Workers Scripts: Edit` scope を含むこと) |

`secret-sync.yml` は `workflow_dispatch` のみ受け付け、かつ `if: github.ref == 'refs/heads/main'`
で main 起動限定 (Phase 2-B 既知 quirks #9 対策)。任意ブランチで dispatch
しても job 段で skip される。

### 9.6 トラブルシュート

- **`ESC env '...' does not contain a non-empty 'workerSecrets' object`**: ESC env
  の YAML 構造が §9.2 規約に合っていない。`esc env get <env>` で `values`
  ツリーを確認し、`workerSecrets` 配下にキーがあるか見る。
- **`wrangler secret bulk` が 401 / 403**: `CLOUDFLARE_API_TOKEN` の scope に
  `Workers Scripts: Edit` が含まれていない可能性。Cloudflare dashboard で token
  scope を確認 (deploy-workers.yml が動いていれば deploy 用 scope は満たすが、
  Cloudflare 側で scope を絞った別 token を使っている場合は要拡張)。
- **`esc env open` が "no such environment"**: ESC env 名の typo か、
  `PULUMI_ACCESS_TOKEN` の発行 org が `sh11235` 以外。`pulumi whoami` で確認。

## 10. 参考

- 設計判断 / 背景: [#675](https://github.com/SH11235/rshogi/issues/675) (umbrella)
- Phase 1 実装単位: [#676](https://github.com/SH11235/rshogi/issues/676)
- Phase 2-D (本 §9): [#690](https://github.com/SH11235/rshogi/issues/690)
- [Pulumi Cloudflare Provider Registry](https://www.pulumi.com/registry/packages/cloudflare/)
- [Pulumi Cloud Individual tier](https://www.pulumi.com/pricing/)
- [Pulumi ESC docs](https://www.pulumi.com/docs/esc/)
- [`pulumi/esc-action`](https://github.com/pulumi/esc-action)
- 既存 wrangler 運用 runbook: [`deployment.md`](deployment.md)
