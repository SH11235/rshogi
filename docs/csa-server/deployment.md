# rshogi-csa-server-workers デプロイ運用 Runbook

Cloudflare Workers 上で `rshogi-csa-server-workers` を運用するための
セットアップと運用手順をまとめる。**初回構築時** は §1〜§3 を順に実行する。
**通常運用** で参照するのは §4〜§6。

本 worker は **staging / production の 2 環境** を Cloudflare 上に分けて運用する:
- **staging**: `main` への push で自動 deploy される canary。runtime regression を
  early に検出する場として使う。
- **production**: 意図的な `workflow_dispatch` でのみ deploy される本番。shogi
  engine 側の改修だけが merge されたときに本番反映が走らないように切り分けている。

## 0. アーキテクチャ概要

staging / production の 2 環境はそれぞれ同じ shape を持つ Worker / DO / R2
構成を独立 namespace で運用する。下図は **1 環境分** のデータフロー:

```
┌──────────────────────────────────────────────────────────────────┐
│ Cloudflare edge — staging / production がそれぞれ同じ shape を持つ │
│                                                                  │
│  ┌──────────────────┐  WS upgrade  ┌─────────────────────────┐  │
│  │ Worker (router)  │ ───────────► │ Durable Object          │  │
│  │ src/router.rs    │              │ (GameRoom, 1 room = 1) │  │
│  │ - Origin check   │              │ src/game_room.rs        │  │
│  │ - id_from_name() │              │ - WS Hibernation        │  │
│  └──────────────────┘              │ - SQLite Storage        │  │
│                                    │ - Alarm API (時間切れ)  │  │
│                                    └────────┬────────────────┘  │
│                                             │ 終局時 PUT          │
│                                             ▼                    │
│                                    ┌────────────────────────┐    │
│                                    │ R2: KIFU_BUCKET        │    │
│                                    │   (CSA V2 棋譜)        │    │
│                                    │ R2: FLOODGATE_HISTORY  │    │
│                                    │   (1 対局 = 1 JSON)    │    │
│                                    └────────────────────────┘    │
└──────────────────────────────────────────────────────────────────┘
```

両環境の差分（同じ shape の各構成要素が、環境ごとに独立した namespace を持つ）:

| 区分 | staging | production |
|---|---|---|
| Worker name | `rshogi-csa-server-workers-staging` | `rshogi-csa-server-workers` |
| `KIFU_BUCKET` | `rshogi-csa-kifu-staging` | `rshogi-csa-kifu-prod` |
| `FLOODGATE_HISTORY_BUCKET` | `rshogi-csa-floodgate-history-staging` | `rshogi-csa-floodgate-history-prod` |
| GameRoom DO storage | 上記 Worker 配下で隔離（互いの SQLite に触れない） | 同左 |
| 起動経路 | `main` への push で自動 deploy | `workflow_dispatch (target=production)` |
| 設定ファイル | `wrangler.staging.toml` | `wrangler.production.toml` |

共通の振る舞い:

- 1 ルーム = 1 Durable Object instance（`id_from_name(room_id)` で決定論解決）
- WebSocket Hibernation を使い、対局アイドル中は worker を停止状態で保持
- 棋譜は R2 (`KIFU_BUCKET`)、Floodgate 履歴は R2 (`FLOODGATE_HISTORY_BUCKET`)
  にそれぞれ書き出す
- Alarm API で時間切れを検知して終局確定

## 1. 必要なもの

| 区分 | 要件 |
|---|---|
| Cloudflare アカウント | **Workers Free プランで本構成は動作**（詳細は §1.1）。本格運用で Free 制限を超える見込みなら Workers Paid を検討 |
| GitHub リポジトリ権限 | Settings → Environments / Secrets and variables への書き込み（Admin 相当） |
| ローカル環境 | [Vite+](https://viteplus.dev/) (Node.js + pnpm を `vp` で一括管理。pnpm version は `package.json` の `packageManager` で pin) / Rust toolchain (`rust-toolchain.toml` で pin) + `wasm32-unknown-unknown` target |

`wrangler` は `crates/rshogi-csa-server-workers/package.json` の devDependency
として `pnpm-lock.yaml` で version を pin 済み。global install ではなくリポジトリ
ローカルに install する経路に統一しているので、`vp install` だけでローカルと
CI のバージョンが一致する。

```bash
# Rust 側 (rust-toolchain.toml で channel pin 済み)
rustup target add wasm32-unknown-unknown
cargo install -q worker-build@^0.8 --locked

# Node 側 (Vite+ が Node + pnpm を一括管理、global install 不要)
curl -fsSL https://vite.plus | bash    # 初回のみ。Vite+ をインストール
cd crates/rshogi-csa-server-workers
vp install                              # `packageManager` の pnpm 経由で deps を install
```

以降 `wrangler` は `vp exec wrangler ...` または scripts (`vp run deploy:staging`
/ `vp run deploy:prod` 等) 経由で呼び出す。`npm install -g wrangler` /
`pnpm add -g wrangler` などの global install は不要。

> 💡 **Vite+ を使わない場合**: `cd crates/rshogi-csa-server-workers && pnpm
> install` で deps を入れた後、`pnpm exec wrangler <command>` または
> `pnpm run deploy:staging` 等で同じ scripts を叩ける。本 runbook は
> `vp` 経由のコマンドを基本形として記述するが、**`vp run X` ↔ `pnpm run X`
> / `vp exec wrangler X` ↔ `pnpm exec wrangler X` の置換が常に成立する**
> （`vp` は内部で `package.json` の `packageManager` の pnpm を呼んでいる
> だけ）。

### 1.1 プラン確認（Free でも動作する）

本構成は Workers Free プランで動作する:

- **SQLite-backed Durable Objects** は Free プランで利用可
  （`[[migrations]] new_sqlite_classes = ["GameRoom"]` 経路）
- **R2** は Free 枠で棋譜出力・Floodgate 履歴の通常運用に十分

ただし以下の Free 制限を恒常的に超える見込みなら Workers Paid (約 $5/月) を
検討する:

| 区分 | Workers Free 上限 |
|---|---|
| Workers requests | 100,000 / 日 |
| Workers CPU time | 10 ms / リクエスト（wall clock 上限は 30 秒。DO の WS は accept_web_socket → Hibernation でリクエスト単位に分解されるので、通常運用では制約にならない） |
| R2 ストレージ | 10 GB / 月 |
| R2 Class A ops（PUT 等） | 10M / 月 |
| R2 Class B ops（GET 等） | 1M / 月 |

> ℹ️ 上記値は 2026-04 時点。Cloudflare 側の plan / limit は予告なく更新される
> ことがあるため、最新値は
> [Cloudflare Workers — Pricing](https://developers.cloudflare.com/workers/platform/pricing/)
> および [R2 — Pricing](https://developers.cloudflare.com/r2/pricing/) を
> 参照すること。

[Workers & Pages → Plans](https://dash.cloudflare.com/?to=/:account/workers/plans)
で現プランを確認可能。CLI からは `vp exec wrangler login` を済ませた後で:

```bash
vp exec wrangler whoami
```

を打つと現在認証中のアカウント情報が出る。

## 2. 初回 Cloudflare / GitHub セットアップ

### 2.1 R2 buckets を作成

staging / production の両方に対して各 2 bucket（合計 4 bucket）を作る。

```bash
cd crates/rshogi-csa-server-workers

# staging
vp exec wrangler r2 bucket create rshogi-csa-kifu-staging
vp exec wrangler r2 bucket create rshogi-csa-floodgate-history-staging

# production
vp exec wrangler r2 bucket create rshogi-csa-kifu-prod
vp exec wrangler r2 bucket create rshogi-csa-floodgate-history-prod
```

bucket 名は `wrangler.staging.toml` / `wrangler.production.toml` の `bucket_name`
と一致させる。命名規約は Cloudflare 側の制約（小文字英数字 + ハイフン、
3〜63 文字、先頭末尾は英数字）に従う。

### 2.2 API token を作成

staging / production それぞれに対して別 token を発行する。

[Cloudflare Dashboard → My Profile → API Tokens](https://dash.cloudflare.com/profile/api-tokens)
から **Create Token** を 2 回繰り返す。

**推奨**: テンプレートから "Cloudflare Workers を編集する" (英語版: "Edit
Cloudflare Workers") を選択する。これだけで Workers Scripts / Workers KV /
Workers R2 / Workers Tail / membership read など実運用に十分な権限が一括
付与される。Account Resources は本リポジトリ用のアカウントのみに絞ること。

token 名は環境を識別できる文字列にする:

| token 名（例） | 用途 |
|---|---|
| `rshogi-csa-server-workers-staging-deploy` | `rshogi-csa-server-workers-staging` Environment の `CLOUDFLARE_API_TOKEN` に登録 |
| `rshogi-csa-server-workers-production-deploy` | `rshogi-csa-server-workers-production` Environment の `CLOUDFLARE_API_TOKEN` に登録 |

> ℹ️ Cloudflare API token のスコープは **アカウント単位**で、token を分けても
> 他環境の Worker を技術的に触れないという強制力はない。あくまで運用上の事故
> 防止 + rotation 容易化（片方が漏れた時に他方を巻き込まずに revoke できる）が
> 目的。

> ℹ️ Cloudflare 現行 API token モデルでは **Durable Objects は独立した
> permission category として存在しない**。DO migration 操作（`new_sqlite_classes`
> 等）は **`Workers スクリプト:編集`** 配下に内包されるため、preset に
> "Durable Objects" 行が無くても本構成の deploy は正常動作する。
>
> preset の権限内訳は Cloudflare 側仕様で更新されることがあるため、token 作成
> 直前のサマリ画面で **`Workers スクリプト:編集`** と **`Workers R2 Storage:編集`**
> の 2 つが含まれていることを確認してから "Create Token" を押す。preset から
> どちらかが脱落していた場合は "Custom token" に切り替えて下表を反映する。

詳細を絞りたい場合は "Custom token" で以下を組み合わせる:

| Scope | Resource | Permission |
|---|---|---|
| Account | Workers Scripts | Edit （DO migration もここで covered）|
| Account | Workers R2 Storage | Edit |
| Account | Workers Tail | Read （`vp run tail:*` で必要）|
| Account | Account Settings | Read |
| User | Memberships | Read |

> ⚠️ 作成した token は **画面遷移すると 2 度と表示されない**。そのまま手元の
> パスワードマネージャ等に控えてから次へ進む。

### 2.3 Account ID を取得

[Cloudflare Dashboard](https://dash.cloudflare.com/) 右下の "Account details"
セクション → "Account ID" をコピー。staging / production で同じ値を使う。

### 2.4 GitHub Environments を構成

`rshogi-csa-server-workers-staging` / `rshogi-csa-server-workers-production` の
2 つの GitHub Environment を作成し、それぞれに **secret と variable の 2 種類**
を登録する設計にする:

- **secrets** (`CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID`): 本セクション
  §2.4.1 / §2.4.2 で登録する
- **variables** (`WORKERS_HEALTH_URL`): §2.6 で初回 deploy 後に登録する

GitHub UI 上は各 Environment の編集画面で `Environment secrets` と
`Environment variables` の 2 タブが並んでいる（同じ画面内）。

Environment ベースにすることで、deploy job 側で
`environment: rshogi-csa-server-workers-staging` /
`environment: rshogi-csa-server-workers-production` を宣言した時に
**その環境に紐付いた secret / variable だけ** が注入される
（cross-env の取り違えが発生しない）。

> ℹ️ Environment 名に crate prefix (`rshogi-csa-server-workers-`) を付けるのは、
> 同 repository 内の他 crate（例: 将来 TCP server が独自 staging を持つ場合）
> と Environment 名が衝突しないようにするため。GitHub Environment は repository
> 単位の namespace なので、単に `staging` / `production` だと別 crate の deploy
> 経路と secret を共有してしまう恐れがある。

[Settings → Environments → New environment]:

#### 2.4.1 `rshogi-csa-server-workers-staging` environment

| 項目 | 値 |
|---|---|
| Name | `rshogi-csa-server-workers-staging` |
| Deployment branches | Selected branches → `main` のみ許可（`main` 以外の ref で `gh workflow run --ref <branch>` を打っても Deployment branches 制限により Environment 解決時に reject され、secret は注入されない gating になる） |
| Required reviewers | （任意。`main` への push で自動 deploy する設計なので unset を推奨） |

[Environment secrets] で:

| Secret | 値 |
|---|---|
| `CLOUDFLARE_API_TOKEN` | §2.2 で作成した staging 用 token |
| `CLOUDFLARE_ACCOUNT_ID` | §2.3 の Account ID |

#### 2.4.2 `rshogi-csa-server-workers-production` environment

| 項目 | 値 |
|---|---|
| Name | `rshogi-csa-server-workers-production` |
| Deployment branches | Selected branches → `main` のみ許可（同上の gating。`main` 以外の ref で dispatch を打てる構成は production secret 漏洩リスクが高い） |
| Required reviewers | **1 名以上の reviewer 設定を推奨**。`workflow_dispatch` を起動できるのは repository write 権限者なので、reviewer を立てておくと dispatch 1 アクションでの誤 deploy を防げる |

[Environment secrets] で:

| Secret | 値 |
|---|---|
| `CLOUDFLARE_API_TOKEN` | §2.2 で作成した production 用 token |
| `CLOUDFLARE_ACCOUNT_ID` | §2.3 の Account ID |

> ℹ️ Repository secret に同名の値が既にある場合、Environment secret が優先
> される。Environment へ移管した後は repository secret を削除して二重管理を
> 解消することを推奨。

#### 2.4.3 移管中の中間状態の挙動

repository secret から Environment secret への移管中は以下のいずれかの状態に
なる。**どの段階で workflow が走っても deploy が壊れない fail-safe 設計**:

| 状態 | 挙動 |
|---|---|
| Environment が **未作成** | GitHub Actions は `environment: rshogi-csa-server-workers-<env>` を宣言した job 起動時に、対象 environment が存在しなければ **default 設定（Deployment branches 制限なし、Required reviewers なし、Environment secret 未登録）で auto-create する**。secret は何も注入されないので preflight が `can_deploy=false` を出して deploy step を skip する。**ただし auto-create された Environment は protection rules が抜けた状態で残るため、§2.4.1 / §2.4.2 の手順で `main` only / Required reviewers を後付け設定する必要がある**（事前に手動作成しておくのが望ましい） |
| Environment **作成済み** + Environment secret **未登録** + repo secret 残存 | `environment:` 宣言した job では Environment secret が同名 repo secret を上書きする仕様。Environment secret が空なので `secrets.*` も空となり、preflight が `can_deploy=false` を出して deploy step を skip する safe fallback が効く。**deploy が壊れることはない**（job は warning と共に成功扱いで終わる） |
| Environment secret **登録済み** | 通常運用。Environment secret が `secrets.*` に注入され、deploy が走る |

つまり **preflight job が必ず fail-safe gate になる** ため、secret 値の登録
忘れや Environment 未作成で本番が落ちるシナリオは無い。ただし auto-create
された Environment は protection rules が抜けるので、`main` only と
Required reviewers の **作成時設定** 自体は §2.4.1 / §2.4.2 の通り重要。

### 2.5 Cloudflare 上で Worker secret を設定する

`wrangler.<env>.toml` の `[vars]` には書かない値を Cloudflare 側で secret
として登録する。本リポジトリで現状必要な secret:

| Secret | 用途 |
|---|---|
| `ADMIN_HANDLE` | `%%SETBUOY` / `%%DELETEBUOY` を許可する運営ハンドル名。OSS repo に handle 名が出ない経路で defense-in-depth を保つ |

設定手順（`vp exec wrangler login` を済ませた後で）:

```bash
cd crates/rshogi-csa-server-workers

# staging
vp exec wrangler secret put ADMIN_HANDLE --config wrangler.staging.toml

# production（staging と異なる値を推奨）
vp exec wrangler secret put ADMIN_HANDLE --config wrangler.production.toml
```

プロンプトに値を入力（echo されない）。例: `rshogi-ops-<random>` のように
一般的でない文字列を選ぶ。staging / production で異なる handle にすることで
「staging で漏れた値が production に通用しない」分離を保つ。

> ℹ️ `ADMIN_HANDLE` は CSA プロトコルの `LOGIN <name> <password>` の `name` 側
> として送信される **平文文字列** で、token 形式ではないため漏洩耐性が handle
> 名のエントロピーに直結する。**最小 16 文字 + 英数記号混在の高エントロピー
> 文字列**（例: `rshogi-ops-<8 桁以上の random>`）を選ぶことを推奨。短い handle
> は brute force の標的になり得る。

> ℹ️ secret の値は Cloudflare 側で encrypted at rest され、CI ログにも commit
> 履歴にも残らない。Worker code は `[vars]` と同じ namespace から
> `env.var(ConfigKeys::ADMIN_HANDLE)` で読む（Cloudflare 仕様）。

| 操作 | コマンド |
|---|---|
| rotation（再設定） | 同じ `secret put` を再度実行すれば上書きされる |
| 削除 | `vp exec wrangler secret delete ADMIN_HANDLE --config wrangler.<env>.toml` |
| 一覧 | `vp exec wrangler secret list --config wrangler.<env>.toml` |

整合性 test (`tests/wrangler_environment_toml_consistency.rs`) が
`wrangler.<env>.toml` の `[vars]` に `ADMIN_HANDLE` が混入していたら CI で
fail させる契約。

### 2.6 (オプション) Health URL を Environment variable に設定

各 Environment の [Variables] tab で:

| Name | 値（例） |
|---|---|
| `WORKERS_HEALTH_URL` | staging: `https://rshogi-csa-server-workers-staging.<your-subdomain>.workers.dev/health`<br>production: `https://rshogi-csa-server-workers.<your-subdomain>.workers.dev/health` |

`<your-subdomain>` は Cloudflare アカウント固有の workers.dev サブドメイン
（例: `your-name`）。**§3.1 / §3.2 の `wrangler deploy` 実行ログ末尾に
`Published ... https://...workers.dev` という形で完全な URL が出力される**ので、
その値を流用する。
[Cloudflare Dashboard → Workers & Pages → 各 worker → Triggers → Routes] でも
確認できる。

これを設定すると、deploy 完了後に CI が `/health` を curl で叩いて smoke check
する step が起動する。値未設定なら smoke step は skip されるだけで deploy 自体
は成功扱い。**§3 の初回 deploy が成功してから設定する** こと。

## 3. 初回手動 deploy

CI 自動 deploy 前に、ローカルから 1 度手動で deploy して動作確認する。
**この手順は最初の 1 度だけ**。以降は CI が自動で deploy する（staging）か、
意図的な dispatch で deploy する（production）。

順序は **staging を先に立てて smoke check で OK を確認 → production を立てる**
にする。staging で発覚した不備が production に伝播しないようにする。

> ⚠️ `wrangler.<env>.toml` の `CORS_ORIGINS` が空のまま deploy すると `/health`
> は応答するが全 WS Upgrade が 403 で拒否される（client が一切繋がらない状態）。
> client 側の Origin が確定するまでは `/health` のみ動く運用で OK。

### 3.1 staging を deploy する

`wrangler.staging.toml` の値を確認する（未設定なら通常の commit / merge 経路で更新する）:

- `[[r2_buckets]] bucket_name` が §2.1 で作成した staging bucket 名と一致しているか
- `[vars] CORS_ORIGINS` に staging client の Origin が入っているか
  （未確定なら空のままで OK。空のままだと全 Upgrade を 403 で拒否する安全側既定）
- 時計設定（`CLOCK_KIND` / `TOTAL_TIME_*` / `BYOYOMI_*`）を運用方針に合わせる
- `ADMIN_HANDLE` を staging Cloudflare secret に登録済みか確認（§2.5）。
  確認: `vp exec wrangler secret list --config wrangler.staging.toml`

```bash
cd crates/rshogi-csa-server-workers
vp exec wrangler login    # 初回のみ
vp run deploy:staging
```

`vp run deploy:staging` は `package.json` で
`wrangler deploy --config wrangler.staging.toml` のショートカット
（Vite+ 非使用時は `pnpm run deploy:staging` で同等。§1 末尾の置換ルール参照）。
`wrangler.staging.toml` の `[build] command = "worker-build --release"` が
`wrangler deploy` の前段で自動実行される。

成功すると `https://rshogi-csa-server-workers-staging.<your-subdomain>.workers.dev/`
にデプロイされ、URL が標準出力に表示される。

#### Smoke check

```bash
SUBDOMAIN=<your-subdomain>
curl "https://rshogi-csa-server-workers-staging.${SUBDOMAIN}.workers.dev/health"
# → "rshogi-csa-server-workers v0.1.0"
```

WebSocket 疎通は別途 ws client で確認:

```bash
# `websocat` が無い場合は `cargo install websocat` か `brew install websocat` で入れる
websocat "wss://rshogi-csa-server-workers-staging.${SUBDOMAIN}.workers.dev/ws/test-room-1" \
  -H "Origin: https://staging.rshogi.example.com"
# 接続が確立すれば OK（"LOGIN ..." を入力すると Worker 側で受理する）
```

成功したら §2.6 で `rshogi-csa-server-workers-staging` Environment の
`WORKERS_HEALTH_URL` を登録する。

### 3.2 production を deploy する

staging で動作確認できたら production も同様に立てる。

`wrangler.production.toml` の値を §3.1 と同様に確認する（bucket 名は §2.1 の
production bucket、`ADMIN_HANDLE` は production secret に登録済み）。

```bash
cd crates/rshogi-csa-server-workers
vp run deploy:prod    # Vite+ 非使用時は `pnpm run deploy:prod`（§1 末尾参照）
```

`https://rshogi-csa-server-workers.<your-subdomain>.workers.dev/` にデプロイされる。
同様に smoke check し、`/health` URL を `rshogi-csa-server-workers-production`
Environment の variable に登録する。

### 3.3 DO migration 確認

`wrangler deployments list` は migration tag を表示しない。確認手段は 2 つ:

**(a) 初回 deploy ログを見る**

§3.1 / §3.2 の `wrangler deploy` 実行ログに以下のような行が出ていれば apply 済み:

```
- new_sqlite_classes: GameRoom
```

**(b) Cloudflare Dashboard で確認**

[Workers & Pages → 各 worker → Settings → Durable Objects] セクションで
`GameRoom` クラスが SQLite-backed として表示されていれば apply 済み。

DO migration は **同 tag を再 apply しても skip される**ので、`tag = "v1"` の
内容を変更する場合は新 tag (`v2` / `v3` ...) を `wrangler.<env>.toml` に
**追加** する。既存 tag を編集しても無視される。staging / production の
migration 履歴は Worker namespace が独立しているため互いに干渉しない。

## 4. 通常運用（自動 deploy）

### 4.1 deploy フロー

```
PR 作成 → CI (rust-ci.yml) で fmt/lint/test/wasm-build 全 pass
       ↓
PR merge to main
       ↓
deploy-workers.yml が起動
       ↓
preflight-staging → deploy-staging → smoke-staging  (自動)
       ↓
（運用判断で）workflow_dispatch (target=production) を実行
       ↓
preflight-production → deploy-production → smoke-production
       ↓
Cloudflare に新版が反映（数秒〜数十秒で全エッジに rollout）
```

push 時は **staging のみ** 自動 deploy される。production への deploy は
意図的な workflow_dispatch でのみ起動する（仕様上、push trigger では
production 側 jobs の `if:` 条件が必ず false になり起動不可）。

deploy が trigger される path（`.github/workflows/deploy-workers.yml` 参照）:

- `crates/rshogi-csa-server-workers/**`
- `crates/rshogi-csa-server/**`
- `crates/rshogi-csa/**`
- `crates/rshogi-core/**`
- `Cargo.toml` / `Cargo.lock`
- `.github/workflows/deploy-workers.yml`

これら以外（docs / TCP only crate / 他 workspace member）の変更では deploy は
起動しない。

### 4.2 production への dispatch deploy

[GitHub → Actions → Deploy Workers → Run workflow → Branch: main →
Deploy target environment: **production** → Run]

または CLI から:

```bash
gh workflow run deploy-workers.yml --ref main -f target=production
```

`rshogi-csa-server-workers-production` Environment に Required reviewers を
設定している場合は、deploy job が承認待ち状態になり、reviewer の approve を
経てから実 deploy step が走る。

### 4.3 staging への手動 redeploy

[GitHub → Actions → Deploy Workers → Run workflow → Branch: main →
Deploy target environment: **staging** → Run]

または:

```bash
gh workflow run deploy-workers.yml --ref main -f target=staging
```

通常は push で自動 deploy されるが、Cloudflare 側の障害復旧後に同 commit を
再 apply したい場合などにこの経路を使う。

## 5. Rollback

### 5.1 直前 version に戻す

```bash
cd crates/rshogi-csa-server-workers

# staging
vp exec wrangler rollback --config wrangler.staging.toml

# production
vp exec wrangler rollback --config wrangler.production.toml
```

確認 prompt が出るので `y` で確定。Cloudflare 側で前 version に切り替わる
（数秒で反映）。

### 5.2 特定 version に戻す

```bash
vp exec wrangler deployments list --config wrangler.<env>.toml
# Version ID をコピー

vp exec wrangler rollback <version-id> --config wrangler.<env>.toml
```

### 5.3 Rollback 後の repo state 同期

`wrangler rollback` は Cloudflare 側のみを巻き戻し、リポジトリの code は
そのまま。rollback したまま放置すると次の自動 deploy で同じ壊れたコードが
再度 apply されるため、rollback で対応した不具合は通常 flow（修正 commit
→ main）で前進させる必要がある。

### 5.4 自動 deploy job が途中で失敗したとき

CI 上の deploy job が失敗 (`wrangler-action` が non-zero) した場合、Cloudflare
側の状態は **失敗時点まで進んでいる可能性** がある（一部 binding 更新だけが
反映された等）。以下の順で復旧する:

1. **失敗ログを確認**
   - GitHub Actions の job log を一読し `Error:` 行で原因を切り分ける
   - `Authentication error (10000)`: §6.3 の token 系
   - `R2 bucket not found`: §6.3 の bucket 系
   - `Migration tag conflict`: §3.3 の migration 同 tag 再 apply（変更が apply
     されたかどうかを Dashboard で確認、必要なら新 tag で出し直す）

2. **Cloudflare 側の現在 version を確認**
   ```bash
   cd crates/rshogi-csa-server-workers
   vp exec wrangler deployments list --config wrangler.<env>.toml
   ```
   最新 version が deploy job 開始 **前** のものなら未反映 → 再実行で OK。
   deploy job 中の途中 version になっていたら次へ。

3. **不整合状態の場合は §5.1 / §5.2 の手順で安定 version に rollback**

4. **修正 commit or 同 commit の workflow 再実行**
   - 設定値の問題 (token / secrets / toml) は修正 commit を main に通す
   - 一過性 (network / Cloudflare 側の障害) なら `gh workflow run` で同 commit を再試行

5. **wrangler tail でクライアント影響を観察**
   ```bash
   vp run tail:staging   # or vp run tail:prod
   ```
   既存接続が切れていないか / 新規接続が成立しているかを確認してから運用復帰。

> 💡 deploy job は staging / production それぞれ別 `concurrency.group` で
> serialize される。失敗 job を放置して次の merge を進めると、同環境の deploy
> が後ろで待つ。失敗の追加調査が必要なら GitHub Actions 画面で当該 deploy job
> を **手動 cancel** してから次に進めること。

## 6. 監視 / トラブルシューティング

### 6.1 ログを見る

```bash
cd crates/rshogi-csa-server-workers
vp run tail:staging   # staging   (Vite+ 非使用時: `pnpm run tail:staging`、§1 末尾参照)
vp run tail:prod      # production (Vite+ 非使用時: `pnpm run tail:prod`)
```

`console_log!` 出力 + 例外が realtime で流れる。終局後の R2 PUT 失敗等は
ここで観測できる。

> ⚠️ 現状は文字列ログのみ。`game_id` 等の構造化フィールドを `wrangler tail`
> で grep / filter できる JSON 形式への移行は未対応（§6.4 参照）。

### 6.2 メトリクスを見る

[Cloudflare Dashboard → Workers & Pages → 各 worker → Metrics]

Requests / errors / CPU time / WS connections 数等。詳細指標 (P99 レイテンシ
等) は §6.4 参照。

### 6.3 よくある問題

#### deploy 失敗: `R2 bucket not found`

- `wrangler.<env>.toml` の `bucket_name` が誤っている、または bucket が
  Cloudflare 上に未作成
- 対処: §2.1 の `vp exec wrangler r2 bucket create` を実行、または toml 側を修正

#### deploy 失敗: `Authentication error (10000)`

- 該当 environment の `CLOUDFLARE_API_TOKEN` の permission 不足、または期限切れ
- 対処: §2.2 で token を再作成し permissions を確認、Environment secret を更新

#### deploy 成功するが対局できない

- DO migration が apply されていない（最初の deploy で必ず apply される）
- `[vars] CORS_ORIGINS` が空 or 誤った Origin → WS Upgrade が 403
- 対処: §6.1 の `vp run tail:*` で wrangler tail を見ながら client から接続して
  4xx/5xx を観測

#### Hibernation が効かない

- `state.accept_web_socket()` 経由でなく標準 WS API を使っているケース
  （現実装では使っていないので発生しない想定）
- DO instance が active connection を持ち続けると Hibernation には入らない
  → 設計通りの挙動

#### 自動 deploy が起動しない

- 該当 environment の preflight job が `can_deploy=false` で skip している
  → §2.4 の Environment secrets を確認
- push の path filter に該当しない → §4.1 の path リストを確認
- production が動かない → これは設計通り。production は `workflow_dispatch`
  でのみ起動する（§4.2）

### 6.4 改善ポイント（未実装）

順序は要件発生に応じて別 PR で着手する:

- **Miniflare smoke E2E ハーネス**: `wrangler dev` 経由で WS Upgrade →
  対局成立 → 終局 → R2 棋譜出力までを自動 smoke test 化する
- **wrangler 3 → 4 系への migration**: deploy 時に `▲ [WARNING] The version
  of Wrangler you are using is now out-of-date` 警告が出る。3.x の EOL を
  見据えて 4 系へ上げる（破壊的変更の確認後）
- **Workers structured logging**: `console_log!` 文字列出力を `game_id` /
  `conn_id` 等の構造化 JSON に置き換え、`wrangler tail` で grep / filter
  可能にする（§6.1 のログ整備）
- **詳細メトリクス**: P99 指し手レイテンシ等の SLO 観測を負荷試験ハーネスと
  合わせて Cloudflare Analytics + 別経路で導入（§6.2 のメトリクス整備）

## 7. 設定ファイル比較

| File | 用途 | 管理 | 環境固有の値（`name` / R2 bucket） | `[vars]` で持つ値 |
|---|---|---|---|---|
| `wrangler.toml.example` | local 開発・新規メンバー向け template | Tracked | — | 公開値（`PRODUCTION_VARS_KEYS`）+ local 専用 placeholder（`LOCAL_DEV_ONLY_VARS_KEYS`、例: `ADMIN_HANDLE`）|
| `wrangler.toml` | 各開発者の local 個人設定 | Gitignored | — | `.example` 由来 |
| `wrangler.staging.toml` | CI 自動 deploy 用 staging 設定 | **Tracked**（§3.1 参照）| `name = "rshogi-csa-server-workers-staging"`、R2 bucket は `rshogi-csa-{kifu,floodgate-history}-staging` | 公開値（`PRODUCTION_VARS_KEYS`）のみ。`LOCAL_DEV_ONLY_VARS_KEYS` は **書かない**（§2.5 で secret 化） |
| `wrangler.production.toml` | CI dispatch deploy 用 production 設定 | **Tracked**（§3.2 参照）| `name = "rshogi-csa-server-workers"`、R2 bucket は `rshogi-csa-{kifu,floodgate-history}-prod` | 公開値（`PRODUCTION_VARS_KEYS`）のみ。`LOCAL_DEV_ONLY_VARS_KEYS` は **書かない**（§2.5 で secret 化） |

`wrangler.staging.toml` / `wrangler.production.toml` を tracked にしている理由は、
bucket 名 / `CORS_ORIGINS` / 時計設定など **機密でないがインフラ仕様として固定
したい値** を全員で共有するため。秘匿情報（API token / account_id /
`ADMIN_HANDLE`）は GitHub Environment secrets（CI 認証用）または Cloudflare
Worker secret（runtime 用）経由で注入し、本ファイルに直接書かない。

整合性 test:
- `tests/wrangler_template_consistency.rs`: `wrangler.toml.example` の `[vars]` が
  `PRODUCTION_VARS_KEYS ∪ LOCAL_DEV_ONLY_VARS_KEYS` と双方向に一致することを assert
- `tests/wrangler_environment_toml_consistency.rs`: `wrangler.staging.toml` /
  `wrangler.production.toml` の `[vars]` が `PRODUCTION_VARS_KEYS` 単独と一致し、
  かつ `LOCAL_DEV_ONLY_VARS_KEYS` の各キーが含まれていないことを assert
  （secret 経路の前提を gate）。staging / production それぞれに対して 5 件、
  合計 10 件の検査が走る。
