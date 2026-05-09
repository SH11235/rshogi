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
| Durable Objects (SQLite-backed) ストレージ | 5 GB / アカウント (進行中対局 / grace registry / challenge registry を全部含む) |
| R2 ストレージ | 10 GB / 月 |
| R2 Class A ops（書込系: PUT / COPY / POST / LIST） | 1M / 月 |
| R2 Class B ops（読込系: GET / HEAD） | 10M / 月 |

> ℹ️ 上記値は 2026-05-09 に Cloudflare 公式ドキュメントで再確認した最新値。
> 2026-04 時点で本表に記載していた「R2 Class A 10M / Class B 1M」は
> [Issue #633](https://github.com/SH11235/rshogi/issues/633) で訂正:
> 公式の正しい値は **Class A 1M / Class B 10M** (書込系の方が ops 制限が厳しく、
> 読込系の方が 10 倍緩い)。Cloudflare 側の plan / limit は予告なく更新される
> ことがあるため、最新値は
> [Cloudflare Workers — Pricing](https://developers.cloudflare.com/workers/platform/pricing/)、
> [R2 — Pricing](https://developers.cloudflare.com/r2/pricing/)、および
> [Durable Objects — Limits](https://developers.cloudflare.com/durable-objects/platform/limits/)
> を参照すること。

### 1.1.1 Floodgate 想定 ops budget の見積もり

Floodgate 相当 (1 日 50 対局想定) の月次 ops 消費を 1 対局あたり概算で:

- **Class A ops/対局**: 対局開始時 live-games-index PUT + 終局時 CSA 本文
  PUT (date_key + by_id) + meta PUT + games-index PUT + Floodgate history
  PUT = **~6 ops/対局**。加えて cron sweep / backfill で同 prefix に
  数回 LIST が乗る (+5 ops 程度、月次合計に按分)。`DeleteObject` は
  Cloudflare R2 で Free operation のため Class A 集計外。
- **Class B ops/対局**: viewer 1 アクセスで list 1 + per-entry get 100 (#636
  の cache 60s で抑制効果込) = **~200 ops** (page size と viewer 数に依存)

50 対局/日 × 30 日 = 1,500 対局/月 の試算:

- Class A: 1,500 × 10 ≈ **15K ops/月** (Free 1M の 1.5%)
- Class B: 1,500 × 200 ≈ **300K ops/月** (Free 10M の 3%)
- DO storage: 終局後 cleanup (Issue #637) 前提で 100 MB 程度を上限想定

**Free 枠は十分余裕がある** が、PR #648 の cache が外れる / viewer scrape
botnet が来る等で Class B が 10x 増えると 30% 帯まで上昇する。alert 閾値
70% を [Issue #625](https://github.com/SH11235/rshogi/issues/625) で配信
連携することで監視を automate する。

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
| Required reviewers | **1 名以上の reviewer 設定を推奨**。`workflow_dispatch` を起動できるのは repository write 権限者なので、reviewer を立てておくと dispatch 1 アクションでの誤 deploy を防げる<br>本 Environment を参照する job は `deploy-production` 1 本に統合済（preflight credentials check / wrangler deploy / smoke check を同 job 内 step として直列実行）。Required reviewers を有効にしても **1 dispatch につき 1 回の承認** で全 step が走る |

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
| Environment が **未作成** | GitHub Actions は `environment: rshogi-csa-server-workers-<env>` を宣言した job 起動時に、対象 environment が存在しなければ **default 設定（Deployment branches 制限なし、Required reviewers なし、Environment secret 未登録）で auto-create する**。secret は何も注入されないので credentials check step が `can_deploy=false` を出して後続 deploy step を skip する。**ただし auto-create された Environment は protection rules が抜けた状態で残るため、§2.4.1 / §2.4.2 の手順で `main` only / Required reviewers を後付け設定する必要がある**（事前に手動作成しておくのが望ましい） |
| Environment **作成済み** + Environment secret **未登録** + repo secret 残存 | `environment:` 宣言した job では Environment secret が同名 repo secret を上書きする仕様。Environment secret が空なので `secrets.*` も空となり、credentials check step が `can_deploy=false` を出して後続 deploy step を skip する safe fallback が効く。**deploy が壊れることはない**（job は warning と共に成功扱いで終わる） |
| Environment secret **登録済み** | 通常運用。Environment secret が `secrets.*` に注入され、deploy が走る |

つまり **credentials check step が必ず fail-safe gate になる** ため、secret 値の登録
忘れや Environment 未作成で本番が落ちるシナリオは無い。ただし auto-create
された Environment は protection rules が抜けるので、`main` only と
Required reviewers の **作成時設定** 自体は §2.4.1 / §2.4.2 の通り重要。

### 2.5 Cloudflare 上で Worker secret を設定する

`wrangler.<env>.toml` の `[vars]` には書かない値を Cloudflare 側で secret
として登録する。本リポジトリで現状必要な secret:

| Secret | 用途 |
|---|---|
| `ADMIN_API_TOKEN` | WS 内 admin command (`%%ADMIN <token>`, [#621](https://github.com/SH11235/rshogi/issues/621)) や将来の HTTP admin endpoint の認可基盤として `crate::admin_auth` から参照する static API token (Floodgate audit [#560](https://github.com/SH11235/rshogi/issues/560))。生成 / rotation / admin client フロー / 旧 `ADMIN_HANDLE` 移行手順は [`docs/csa-server/admin_auth.md`](admin_auth.md) を参照。 |

設定 / rotation / admin client フローはすべて
[`docs/csa-server/admin_auth.md`](admin_auth.md) に集約してある (token 生成、
secret 登録、rotation、削除、確認、`%%ADMIN` 受信時の応答仕様までカバー)。

整合性 test (`tests/wrangler_environment_toml_consistency.rs`) が
`wrangler.<env>.toml` の `[vars]` に `ADMIN_API_TOKEN` 等
[`ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS`](../../crates/rshogi-csa-server-workers/src/config.rs)
の値が混入していたら CI で fail させる契約。

> ℹ️ 旧 `ADMIN_HANDLE` (handle 自称 → admin 権限) は
> [#621](https://github.com/SH11235/rshogi/issues/621) で廃止された。
> Cloudflare 側に残った secret は
> `wrangler secret delete ADMIN_HANDLE --config wrangler.<env>.toml` で
> 除去できる ([`admin_auth.md`](admin_auth.md) §「旧 ADMIN_HANDLE からの移行」)。

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

これを設定すると、deploy 完了後に CI が `/health` を curl で叩いて 200 を確認する
smoke step が走る。値未設定でも smoke job は起動するが、step 内で
`::warning::WORKERS_HEALTH_URL not set on <Environment> Environment; skipping smoke check`
を出して `exit 0` で終わるため、CI 全体は成功扱いを維持したまま deploy 健全性
チェックだけ skip される。**§3 の初回 deploy が成功してから設定する** こと。

> ℹ️ 実装メモ: GitHub Actions の job-level `if:` は `environment:` 宣言の
> 解決 **前** に評価されるため、`if: ${{ vars.WORKERS_HEALTH_URL != '' }}`
> のような形では Environment variable が見えず常に false と評価される。
> このため smoke job は job レベルで gate せず、必ず起動して step 内 env で
> 解決した値を見て分岐する設計にしている。

## 3. 初回手動 deploy

CI 自動 deploy 前に、ローカルから 1 度手動で deploy して動作確認する。
**この手順は最初の 1 度だけ**。以降は CI が自動で deploy する（staging）か、
意図的な dispatch で deploy する（production）。

順序は **staging を先に立てて smoke check で OK を確認 → production を立てる**
にする。staging で発覚した不備が production に伝播しないようにする。

> ⚠️ `WS_ALLOWED_ORIGINS` はブラウザ経由（Origin ヘッダ付き）の WS Upgrade に対する
> CSRF 防御リストとして機能する。空のまま deploy すると **ブラウザからの Upgrade は
> 全 403** だが、Origin ヘッダを送らないネイティブ CSA クライアント
> （`rshogi-csa-client` など）は素通しで対局できる。web client 化が確定してから
> §3.5 の手順で Origin を追加すれば足りる。

### 3.1 staging を deploy する

`wrangler.staging.toml` の値を確認する（未設定なら通常の commit / merge 経路で更新する）:

- `[[r2_buckets]] bucket_name` が §2.1 で作成した staging bucket 名と一致しているか
- `[vars] WS_ALLOWED_ORIGINS` に staging client の Origin が入っているか
  （未確定なら空のままで OK。空のままでもネイティブ CSA クライアントは Origin 欠落で
  素通し。web client 化したときだけ Origin の追加が必要）
- 時計設定（`CLOCK_KIND` / `TOTAL_TIME_*` / `BYOYOMI_*`）を運用方針に合わせる
- `ADMIN_API_TOKEN` を staging Cloudflare secret に登録済みか確認（§2.5 / [`admin_auth.md`](admin_auth.md)）。
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

# (a) ネイティブ CSA クライアント経路（Origin ヘッダなし）の確認:
websocat "wss://rshogi-csa-server-workers-staging.${SUBDOMAIN}.workers.dev/ws/test-room-1"
# 接続が確立すれば OK。Origin が無いリクエストは allowlist に関係なく素通しする。

# (b) ブラウザ経路（Origin 付き）の allowlist 確認:
websocat "wss://rshogi-csa-server-workers-staging.${SUBDOMAIN}.workers.dev/ws/test-room-1" \
  -H "Origin: https://csa-client-local"
# `wrangler.staging.toml` の `WS_ALLOWED_ORIGINS` に含まれている Origin を指定する。
# 含まれない値を渡すと `403 Forbidden Origin` で拒否される（allowlist の挙動確認）。
# 接続が確立すれば OK（"LOGIN ..." を入力すると Worker 側で受理する）。
```

成功したら §2.6 で `rshogi-csa-server-workers-staging` Environment の
`WORKERS_HEALTH_URL` を登録する。

### 3.2 production を deploy する

staging で動作確認できたら production も同様に立てる。

`wrangler.production.toml` の値を §3.1 と同様に確認する（bucket 名は §2.1 の
production bucket、`ADMIN_API_TOKEN` は production secret に登録済み）。

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

### 3.4 DO schema 変更時の dry-run / rollback 手順

GameRoom DO は SQLite-backed で、`state.storage().sql().exec(SCHEMA_SQL)`
([`crates/rshogi-csa-server-workers/src/game_room.rs`](../../crates/rshogi-csa-server-workers/src/game_room.rs)
の `SCHEMA_SQL`) を **DO instance 構築時に毎回 exec する**。一方
`wrangler.<env>.toml` の `[[migrations]]` (§3.3) は DO class / binding
レイヤの migration で、新 SQLite クラスの追加や旧クラスの削除を宣言する。
両者はレイヤが異なる:

| レイヤ                       | 宣言場所                              | apply タイミング              |
|------------------------------|---------------------------------------|-------------------------------|
| DO class / binding migration | `wrangler.<env>.toml` `[[migrations]]` | `wrangler deploy` 実行時 (1 回) |
| DO instance 内 SQLite schema | `SCHEMA_SQL` (`game_room.rs`)         | DO instance 構築のたび        |

§3.3 が前者の forward-only 性を扱うのに対し、本節は後者
(`SCHEMA_SQL`) の dry-run と rollback を扱う。

#### 3.4.1 additive-only contract

`SCHEMA_SQL` は **DO instance 構築のたびに毎回そのまま exec される** ため、
直接書ける DDL は **再 exec で no-op になる冪等な statement** に限定する:

- `CREATE TABLE IF NOT EXISTS <name> ( ... )` のみ許可

以下は `SCHEMA_SQL` への直書き **禁止** (本 contract は CI test で gate される。§3.4.4 参照):

- `ALTER TABLE ... ADD COLUMN ...`: SQLite では同名 column 存在時に
  `duplicate column name` で fail するため、再 exec の冪等性を満たさない。
  column 追加が必要な場合は `SCHEMA_SQL` ではなく **guarded migration helper**
  で行う (`PRAGMA table_info` で存在確認してから ADD COLUMN するか、
  `try_exec` 相当で duplicate error を握り潰す経路を別レイヤに用意する)。
  helper 経路はまだ未実装で、必要になった時点で別 Issue で設計する。
- `DROP TABLE` / `DROP COLUMN` / `DROP INDEX`
- `ALTER TABLE ... RENAME` / `RENAME TABLE`
- `ALTER COLUMN <col> TYPE ...` 等の column 型変更
- `TRUNCATE` / `DELETE` 等のデータ書換系
- `CREATE INDEX` (今回の contract では未許可。必要になったら測定と
  rollback 方針込みで contract を拡張する)

destructive 変更は **既に各 DO instance に書き込まれた SQLite データを失う**
ため、`wrangler rollback` で Worker code を巻き戻しても schema 側は
戻らない (§3.4.3)。

#### 3.4.2 dry-run 手順

新しい DDL を `SCHEMA_SQL` に追加する PR を作る前に、以下を確認する:

1. **CI gate を通る**:
   `cargo test -p rshogi-csa-server-workers --test do_schema_additive_contract`
   が pass すること。これは §3.4.1 の additive-only contract に違反して
   いないことを保証する (`--test <name>` で integration test binary を
   名指し実行する。`--tests` (複数形) と末尾 filter の組み合わせは関数名
   filter として解釈され 0 tests になるので注意)。
2. **空 DO で初回 exec が通る**:
   ```bash
   cd crates/rshogi-csa-server-workers
   vp run dev   # wrangler dev を起動 (Vite+ 非使用時は pnpm run dev)
   ```
   別シェルから新規 `room_id` で `/ws/<room_id>` に接続し、初回 fetch で
   `SCHEMA_SQL` が exec される (`game_room.rs::DurableObject::new`)。
   `wrangler tail` 相当の dev console にエラーが出ないことを確認する。
3. **再接続で再 exec が冪等**:
   `wrangler dev` を一度停止 → 起動し直してから同じ `room_id` で再接続
   する。新 DO instance が構築されて `SCHEMA_SQL` が再 exec される。
   `CREATE TABLE IF NOT EXISTS` のみであれば 2 回目以降の exec は
   no-op になる。エラーになる場合は contract に違反した DDL が含まれて
   いるサインなので、§3.4.1 を再確認する。
   > 同じ `room_id` で接続を切って即再接続しても DO isolate が再利用
   > される可能性があり、必ずしも `new()` が再呼び出しされるとは限らない。
   > 再 exec を確実にするには `wrangler dev` 自体を再起動すること。
4. **新 column / 新 table を読む code パスが NULL safe**:
   `SCHEMA_SQL` に table 追加した PR では、Rust 側の読み取りコードが
   旧 schema (= 当該 table が空 / 旧 row) を読んだときに `None` /
   default 値で動くことを `cargo test -p rshogi-csa-server-workers
   --tests` で確認する。これは §3.4.3 の rollback で必須の前提条件。

#### 3.4.3 rollback の前提条件

**重要**: `wrangler rollback` (§5.1) は Worker code 側の version 巻き戻し
であり、**各 DO instance に既に適用された SQLite schema は undo されない**。
旧 column の追加 (`ALTER TABLE ADD COLUMN`) は DO に残ったままなので、
rollback 先の旧 code もそのまま動かす必要がある。

したがって、rollback で安全に戻すための **本体側の準備** は次の 2 点:

- 新 column は code 上 **NULL safe / optional read** にする。具体的には
  `Option<T>` で受けて `.unwrap_or_default()` などで吸収するか、
  `serde(default)` で旧 row も読めるようにする。
- 旧 code が新 column の存在を許容する (read 時に無視する) こと。
  SQLite は宣言外 column を select 句で参照しなければ自然に無視する
  ため、追加した column を必須参照にしない作りなら追加対応不要。

これらが満たされていれば、`wrangler rollback` で前 version の Worker
code に戻しても、各 DO instance に残った新 schema との不整合は起きない。

#### 3.4.4 additive-only contract の CI gate

`crates/rshogi-csa-server-workers/tests/do_schema_additive_contract.rs`
が `SCHEMA_SQL` を簡易 parse し、§3.4.1 の contract に違反する DDL が
含まれていないことを CI で gate する。具体的には:

- `CREATE TABLE` は `IF NOT EXISTS` を必須化
- `ALTER` / `DROP` / `TRUNCATE` / `RENAME` の単語境界出現を destructive
  または非冪等 keyword として fail (`ALTER TABLE ADD COLUMN` も `SCHEMA_SQL`
  直結では fail; 必要時は guarded migration helper 経路に分離する)
- 先頭が未知の statement (例: `CREATE INDEX`, `CREATE TRIGGER`) も fail

contract を拡張する必要が出たら、test 側を更新するのと doc §3.4.1 の
許可 DDL リスト更新を **同じ PR** で行うこと (gate と doc がずれると
事故が再発する)。

#### 3.4.5 破壊的変更が必要な場合 (parallel-run)

どうしても column 削除 / 型変更が必要になった場合、`SCHEMA_SQL` を
直接書き換えるのではなく、以下の **並行運用 (parallel-run) 戦略** を
取る:

1. 新 SQLite クラスを追加 (例: `GameRoomV2`) し、`wrangler.<env>.toml`
   の `[[migrations]]` に新 tag (`v3` 等) で `new_sqlite_classes` を
   追加する。
2. 既存 `GameRoom` クラスは破壊的変更を加えず、新規 room を新クラス
   側に routing する経路を実装する。
3. 既存 DO instance が自然に消化されるのを待つ (1 部屋 = 1 DO instance
   の終局を待つ)。
4. 全クラスからの参照が無くなった段階で旧クラスを `[[migrations]]`
   `deleted_classes` で deprecation する。

blast radius は room 単位 (1 room = 1 GameRoom DO instance) だが、
永続 storage / schema の消滅タイミングを rollback 手順の前提にしない
こと。

### 3.5 web client 化時に `WS_ALLOWED_ORIGINS` を追加する

ネイティブ CSA クライアント (`rshogi-csa-client` 等) は Origin ヘッダを送らないため
allowlist 設定に関係なく素通しで対局できる。一方ブラウザ経由（Origin ヘッダ付き）の
WS Upgrade は `WS_ALLOWED_ORIGINS` に完全一致したときだけ通過する（CSRF 防御）。
production / staging を web client 化したくなったら、本節の手順で Origin を追加する。

#### 3.5.1 標準フロー (新規 web client URL を追加)

1. `wrangler.<env>.toml` の `[vars] WS_ALLOWED_ORIGINS` に追加する Origin を CSV で書く。
   既存値が空文字列なら新値で置換、既存値があれば末尾にカンマ区切りで append する。
   例: `WS_ALLOWED_ORIGINS = "https://rshogi.example.com,https://www.rshogi.example.com"`
2. PR を作成し CI (rust-ci.yml) を通過させる。`tests/wrangler_environment_toml_consistency.rs`
   が `[vars]` キーの整合性を gate しているため、key 名のタイポは PR 段階で落ちる。
3. main へ merge すると §4.1 のフロー (staging は自動 / production は dispatch) で
   新値が deploy される。
4. デプロイ後、§3.1 の WebSocket 疎通 smoke check (b) で対象 Origin から 101 Upgrade が
   返ることを確認する。allowlist 外からのリクエストは引き続き 403 で拒否される。

#### 3.5.2 staging 先行検証ルール

新 Origin を production に直接追加するのではなく、必ず staging で先に検証する:

1. `wrangler.staging.toml` に新 Origin を append する PR を作成 (production には触れない)。
2. main merge → staging 自動 deploy → §3.1 staging smoke check で 101 確認。
3. 24 時間程度 staging で実機通電させる (csa_client や手元の web client から接続)。
   「問題なし」の判定基準: §3.1 smoke check (b) で対象 Origin から 101 Upgrade、
   許可外 Origin から 403、`vp exec wrangler tail` でエラー無し、staging で
   1 局以上完走して R2 棋譜が書き出される、の 4 点。
4. 問題なければ `wrangler.production.toml` に同じ Origin を追加する PR を作成し、
   §4.2 の `workflow_dispatch (target=production)` で deploy。

「同一 PR で staging / production 両方を更新」する誘惑があるが、production 単独で問題が
出た場合 (例: subdomain ミスマッチ) に rollback 範囲を staging に限定できなくなるため
**禁止**。staging が過去 1 deploy 以上問題なく稼働している状態で production を進める。

#### 3.5.3 既存接続への影響と無停止性

- **追加 (Origin を `WS_ALLOWED_ORIGINS` に append)**: 既存接続は影響なし。新 Origin から
  の **新規 Upgrade** だけが許可されるようになる。Cloudflare Workers の rolling deploy は
  数秒〜数十秒で全エッジに伝播するが、伝播中の旧版エッジに新 Origin から接続したリクエスト
  は一時的に 403 になる。再接続で解消する。web client 実装側は **指数バックオフ
  (例: 1s/2s/4s で最大 3 回リトライ)** を入れておくと UX 影響を最小化できる。
- **削除 (Origin を `WS_ALLOWED_ORIGINS` から取り除く)**: 既存 WS 接続 (= DO Hibernation
  に乗っている既存対局) は **切れない**。Origin 検査は新規 Upgrade 時にのみ走るため、
  既存対局は終局まで継続できる。新規接続だけが 403 で拒否される。
- **空に戻す (`WS_ALLOWED_ORIGINS = ""`)**: 上記 "削除" と同じ挙動。ブラウザ経由の新規
  Upgrade はすべて 403。ネイティブ CSA クライアントは引き続き素通し。

#### 3.5.4 incident response (allowlist 緊急停止)

ブラウザ経由の悪意ある接続が殺到しているなど緊急停止したい場合:

1. `wrangler.production.toml` の `WS_ALLOWED_ORIGINS = "..."` を `""` (空文字列) に変える PR
   を作成。タイトルに `[incident]` を含めるとレビュー追跡しやすい。
2. CI 通過後 `gh pr merge --squash` で main に入れる。
   ※ CI / Required reviewers 待ちが許容できない緊急度の場合は §5.2 の
   `wrangler rollback` を **先に** 実行してブラウザ接続を即時遮断し、その後 (a)
   経路で repo state を同期させる (§3.5.5(b) と同じ手順)。
3. §4.2 の `workflow_dispatch (target=production)` で即時 deploy (通常の dispatch 経路と同じ)。
4. §3.5.3 のとおり既存対局は切れず、新規ブラウザ Upgrade だけが 403 になる。ネイティブ
   CSA クライアントはこの incident response の影響を受けないため、対局運用の中核は継続できる。
5. 原因解消後、§3.5.1 の標準フローで Origin を再追加する。

#### 3.5.5 rollback (新 Origin が問題を起こした場合)

allowlist 変更後の rollback には 2 経路ある。状況に応じて使い分ける:

**(a) `git revert` 経路 (本節、推奨既定)**: 真実源の `wrangler.<env>.toml` を旧状態に
巻き戻す PR を作成して再 deploy。後続の自動 deploy で再 apply されるリスクがない。

```bash
# 例: 直前の allowlist 変更 commit を revert
git checkout main && git pull
git log --oneline -10   # allowlist 変更 commit の SHA を確認
git revert <commit-sha>
gh pr create --title "revert: WS_ALLOWED_ORIGINS rollback"
# CI 通過 → merge → §4.2 workflow_dispatch で production redeploy
```

**(b) `wrangler rollback` 経路 (§5.2 参照)**: Cloudflare 側の前 version snapshot に
即時で巻き戻す。CI / merge 待ちなしで秒単位で復旧できるが、repo の `wrangler.<env>.toml`
は変更前のまま残るため、次の自動 deploy で再 apply されて元に戻る。緊急避難として
使い、その後 (a) で repo state を同期させる必要がある (§5.3 と同じ手順)。

いずれの経路も §3.5.3 の "削除" 挙動なので既存対局は切れない。新規接続だけが旧 allowlist
基準に戻る。

## 4. 通常運用（自動 deploy）

### 4.1 deploy フロー

```
PR 作成 → CI (rust-ci.yml) で fmt/lint/test/wasm-build 全 pass
       ↓
PR merge to main
       ↓
deploy-workers.yml が起動
       ↓
deploy-staging job が起動 (自動):
  credentials check → wasm build → wrangler deploy → smoke check
       ↓
（運用判断で）workflow_dispatch (target=production) を実行
       ↓
deploy-production job が起動:
  credentials check → wasm build → wrangler deploy → smoke check
       ↓
Cloudflare に新版が反映（数秒〜数十秒で全エッジに rollout）
```

各 deploy job は preflight (credentials check) / wrangler deploy / smoke check
を 1 job 内 step として直列実行する設計。`environment:` 宣言を 1 job に集約する
ため Required reviewers を有効化しても **1 dispatch につき 1 回の承認**で済む。

push 時は **staging のみ** 自動 deploy される。production への deploy は
意図的な workflow_dispatch でのみ起動する（仕様上、push trigger では
`deploy-production` job の `if:` 条件が必ず false になり起動不可）。

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

> ⚠️ rollback には 2 経路ある。**default は `git revert` (§5.1)**。
> `wrangler rollback` (§5.2) は緊急避難用で、Cloudflare 側のみを巻き戻すため
> main 同期 PR を 24h 以内に必ず出すこと。drift detection workflow
> (`cloudflare-drift-check.yml`) が 1 日 1 回 staging の `/health.deployed_sha`
> と main の deploy-trigger commit を突合し、乖離があれば `csa-deploy-drift`
> label の issue を起票する ([Issue #639](https://github.com/SH11235/rshogi/issues/639))。

### 5.1 (default) `git revert` 経路で main を巻き戻す

`wrangler rollback` と異なり、main 上の code 自体を巻き戻すので次の自動 deploy
が同じ壊れたコードを再 apply することはない。staging は merge 直後に自動
deploy されるため staging の `/health.deployed_sha` が revert commit を指せば
復旧完了。production は revert PR merge 後に dispatch deploy で反映する (§4.2)。

```bash
# 1. revert PR を出す
git fetch origin main
git checkout -b revert/<bad-sha> main
git revert <bad-sha>
git push -u origin revert/<bad-sha>
gh pr create --title "revert: <subject>" \
  --body "Reverts <bad-sha>. Refs #<issue>."

# 2. merge 後 staging /health の deployed_sha が revert commit を指すことを確認
curl -fsS https://<staging-host>/health | jq -r .deployed_sha
# => 期待: 上記 revert commit の sha (実際は revert を含む最新 deploy-trigger commit)

# 3. production も同期する場合は dispatch deploy
gh workflow run deploy-workers.yml --ref main -f target=production
```

### 5.2 (緊急避難) `wrangler rollback` で Cloudflare 側だけ巻き戻す

production 障害で **次の deploy を待てない** 場合のみ使用。Cloudflare 側の
version を即時切り替えるが repo 側は変わらないので、24h 以内に必ず §5.1 の
revert PR を出して main を同期する。同期しないと:

- 次の自動 deploy (staging) で同じ壊れたコードが再 apply される
- drift detection workflow が翌日 issue を起票する
- production は次の dispatch deploy で壊れた版が再投入される race が残る

#### 5.2.1 直前 version に戻す

```bash
cd crates/rshogi-csa-server-workers

# staging
vp exec wrangler rollback --config wrangler.staging.toml

# production
vp exec wrangler rollback --config wrangler.production.toml
```

確認 prompt が出るので `y` で確定。Cloudflare 側で前 version に切り替わる
（数秒で反映）。完了したら直ちに §5.1 の revert PR フローに進む。

#### 5.2.2 特定 version に戻す

```bash
vp exec wrangler deployments list --config wrangler.<env>.toml
# Version ID をコピー

vp exec wrangler rollback <version-id> --config wrangler.<env>.toml
```

### 5.3 Rollback 後の repo state 同期 (drift 検知)

`.github/workflows/cloudflare-drift-check.yml` が **staging のみ** を 1 日 1 回
監視する:

- `/health` JSON の `deployed_sha` (deploy 時に CI が注入した
  `DEPLOY_TRIGGER_SHA` = `git log -1 --format=%H -- <deploy-trigger paths>`) と、
  main 上の同 path リストでの最新 commit を比較
- 乖離があれば 5 分待って再 fetch (Cloudflare の rollout 伝播 lag を吸収)
- それでも乖離があれば `csa-deploy-drift` label の open issue を 1 件持つ
  (なければ新規作成、あれば comment 追記)

> 💡 比較対象に `origin/main` HEAD ではなく **deploy-trigger path に触れた最新
> commit** を使うのは、docs-only commit が main HEAD にあっても false positive
> にならないため。`deploy-workers.yml` の `push.paths` と同じリストを drift
> workflow / `Compute DEPLOY_TRIGGER_SHA` step で揃えるのが整合の前提
> (path 変更時は両 workflow を同 PR で更新する)。

issue が立った場合の対処:

1. issue 本文の `git log <actual>..<expected>` で main 上に積まれた未 deploy
   commit を確認
2. §5.1 の revert PR が必要か判断 (rollback 後の同期忘れなら revert で前進)
3. revert PR merge 後、staging の `/health.deployed_sha` が一致することを確認
4. drift issue を close

`cloudflare-drift-check.yml` は `csa-deploy-drift` label を `gh label create`
で idempotent に確保するため、初回 cron 実行で label が自動作成される (repo
設定で事前作成しておいても良い)。

### 5.4 production の手動 drift 同期確認

production は `workflow_dispatch` でしか deploy されないため自動 drift 監視
の対象外。`wrangler rollback` を production に対して使った後は、main 同期
PR を merge した時点で **手動** で以下を確認する:

```bash
# Cloudflare 側 current version の sha (deployment 一覧から最新)
cd crates/rshogi-csa-server-workers
vp exec wrangler deployments list --config wrangler.production.toml

# production /health の deployed_sha と main の deploy-trigger commit を突合
curl -fsS https://<production-host>/health | jq -r .deployed_sha
git log -1 --format=%H origin/main -- \
  crates/rshogi-csa-server-workers \
  crates/rshogi-csa-server \
  crates/rshogi-csa \
  crates/rshogi-core \
  Cargo.toml Cargo.lock .github/workflows/deploy-workers.yml
# 上記 2 つが一致していれば同期 OK。乖離があれば dispatch deploy を流す。
```

production を自動 drift 監視に乗せたくなったら
`cloudflare-drift-check.yml` に第 2 job を追加する。第 1 版で staging only に
した理由は dispatch deploy のみで自動再 apply の race が起きにくく、頻度が
低いため。

> ⚠️ DO schema 変更 (`SCHEMA_SQL`) を含む rollback の場合、Worker code は
> 戻っても各 DO instance に既に適用された SQLite schema は **戻らない**。
> 詳細と対処は §3.4.3 を参照。

### 5.5 自動 deploy job が途中で失敗したとき

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

3. **不整合状態の場合は §5.2 (`wrangler rollback`) の手順で安定 version に rollback**
   その後 §5.1 (`git revert`) で main 側を同期する。

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
- `[vars] WS_ALLOWED_ORIGINS` が空 or 誤った Origin → ブラウザ経由（Origin 付き）の
  WS Upgrade が 403（ネイティブ CSA クライアントは Origin 欠落で素通し継続）
- 対処: §6.1 の `vp run tail:*` で wrangler tail を見ながら client から接続して
  4xx/5xx を観測

#### Hibernation が効かない

- `state.accept_web_socket()` 経由でなく標準 WS API を使っているケース
  （現実装では使っていないので発生しない想定）
- DO instance が active connection を持ち続けると Hibernation には入らない
  → 設計通りの挙動

#### 自動 deploy が起動しない

- 該当 environment の credentials check step が `can_deploy=false` を出力して
  後続 step が skip されている → §2.4 の Environment secrets を確認
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
| `wrangler.toml.example` | local 開発・新規メンバー向け template | Tracked | — | 公開値（`SHARED_PUBLIC_VARS_KEYS`）+ local 専用 placeholder（`LOCAL_DEV_ONLY_VARS_KEYS`、例: `ADMIN_API_TOKEN`）|
| `wrangler.toml` | 各開発者の local 個人設定 | Gitignored | — | `.example` 由来 |
| `wrangler.staging.toml` | CI 自動 deploy 用 staging 設定 | **Tracked**（§3.1 参照）| `name = "rshogi-csa-server-workers-staging"`、R2 bucket は `rshogi-csa-{kifu,floodgate-history}-staging` | 公開値（`SHARED_PUBLIC_VARS_KEYS`）のみ。`LOCAL_DEV_ONLY_VARS_KEYS` は **書かない**（§2.5 で secret 化） |
| `wrangler.production.toml` | CI dispatch deploy 用 production 設定 | **Tracked**（§3.2 参照）| `name = "rshogi-csa-server-workers"`、R2 bucket は `rshogi-csa-{kifu,floodgate-history}-prod` | 公開値（`SHARED_PUBLIC_VARS_KEYS`）のみ。`LOCAL_DEV_ONLY_VARS_KEYS` は **書かない**（§2.5 で secret 化） |

`wrangler.staging.toml` / `wrangler.production.toml` を tracked にしている理由は、
bucket 名 / `WS_ALLOWED_ORIGINS` / 時計設定など **機密でないがインフラ仕様として固定
したい値** を全員で共有するため。秘匿情報（API token / account_id /
`ADMIN_API_TOKEN`）は GitHub Environment secrets（CI 認証用）または Cloudflare
Worker secret（runtime 用）経由で注入し、本ファイルに直接書かない。

整合性 test:
- `tests/wrangler_template_consistency.rs`: `wrangler.toml.example` の `[vars]` が
  `SHARED_PUBLIC_VARS_KEYS ∪ LOCAL_DEV_ONLY_VARS_KEYS` と双方向に一致することを assert
- `tests/wrangler_environment_toml_consistency.rs`: `wrangler.staging.toml` /
  `wrangler.production.toml` の `[vars]` が `SHARED_PUBLIC_VARS_KEYS` 単独と一致し、
  かつ `LOCAL_DEV_ONLY_VARS_KEYS` の各キーが含まれていないことを assert
  （secret 経路の前提を gate）。staging / production それぞれに対して 5 件、
  合計 10 件の検査が走る。

## 591 hotfix deploy 手順 (#601 配線前の手動手順)

[Issue #591](https://github.com/SH11235/rshogi/issues/591) の partial fix (新規対局で `Reconnect_Token:` 拡張行の配布を
`RECONNECT_GRACE_SECONDS` の値に応じて gate する) を deploy する際の追加手順。

### 過渡期 in-flight 対局の影響範囲

本 hotfix は **新規 `start_match` 経由の対局で確定的に `reconnect_rejected`
ABNORMAL を解消** するが、deploy 直前から進行中の対局は引き続き
`PersistedConfig.{black,white}_reconnect_token = Some(...)` を保持しており、
deploy 後の WS 切断で旧 bug を踏みうる。

worst case 対局時間 (production CLOCK 設定 `countdown` + `total_time_sec=600`
+ `byoyomi_sec=10` + `max_moves=256`):

- 両者がほぼ全 main time を使い切り、`max_moves` 上限まで byoyomi 上限 (10 秒/手) を消費するケース
- `total_time_sec × 2 + byoyomi_sec × max_moves = 600 × 2 + 10 × 256 = 1200 + 2560 = 3760 秒 ≈ 約 63 分`
- 実運用ではほとんど到達しないが timeout 設計の上限根拠として用いる。典型値は 25〜30 分

### deploy 前確認手順

1. `curl https://rshogi-csa-server.sh11235.com/api/v1/games/live` で in-flight
   対局が無いことを確認
2. live が空なら `wrangler deploy` 実行 (CI workflow_dispatch でも同じ)
3. live が残っている場合は終局を待ってから deploy する (smoke 用途のみで通常は
   数件以内に収まる想定)

### graceful drain は未配線 (→ §12 で best-effort drain 配線済)

`wrangler deploy` で進行中の DO instance を「対局終了後にだけ replace する」
真の graceful drain は Workers の制約上配線できない (DO state を新 isolate に
migrate する公式経路がないため)。代わりに **§12 で best-effort drain (deploy
前に in-flight 対局がゼロになるまで待機する経路) を自動化** している。Issue
#601 で workflow_dispatch + script 化済 (本節の「手動 curl で live API を見る」
旧手順は §12.2 の自動化経路に置き換わった)。

### AGREE 待ち state 残存の制限

Workers DO 側に AGREE 待ち TTL 未配線 (followup-H で対応予定)。AGREE 待ちの
まま stuck した対局は live API に出ないが DO 上に残る。本 hotfix は AGREE
後の対局のみ救済する点に留意。

### `RECONNECT_GRACE_SECONDS` の env 上げ前の前提

production の `RECONNECT_GRACE_SECONDS` を `0 → 30` に上げる (= reconnect
protocol の本格有効化、[Issue #591](https://github.com/SH11235/rshogi/issues/591) の Expected 完了条件 (1)) は **followup-C
完了後でないと安全でない**。理由は現状 `PersistedConfig.reconnect_grace_ms`
が永続化されておらず、deploy 直前から in-flight だった対局の grace 値が
`websocket_close` 時に新 env 値で上書きされる race を持つため (旧 token と
新 grace の組合せで未定義挙動)。

followup-C で `PersistedConfig` に grace 値を永続化し、`websocket_close` 側
が env でなく persisted config を読む経路に切り替えてから初めて env 値の
昇格が安全になる。具体順序は以下:

1. followup-A: `enter_grace_window` の alarm tag↔body 整合 + cleanup 集約
2. followup-B: 2 段 put / delete / set_alarm の transaction 化 (worker 0.8.1
   transaction API)
3. followup-C: `PersistedConfig.reconnect_grace_ms` 永続化
4. followup-D: `PendingAlarmKind::TurnDeadline` variant 追加
5. followup-E ([Issue #591](https://github.com/SH11235/rshogi/issues/591) 完了条件): `RECONNECT_GRACE_SECONDS=30` +
   `ALLOW_FLOODGATE_FEATURES=true` への昇格

### deploy 後の確認

`csa-smoke` skill で 1 局走らせ、以下を確認:

- production: `Game_Summary` 内に `Reconnect_Token:` 行が **出ない**
- production: 対局途中で client を切断しても `LOGIN:incorrect reconnect_rejected`
  ループに入らず、`#ABNORMAL` で素直に終局する
- staging: `RECONNECT_GRACE_SECONDS=30` で運用している場合は `Reconnect_Token:`
  行が **出る** (= 既存 reconnect 経路の無回帰確認)

## 12. deploy drain (best-effort)

production deploy 直前に live (進行中) 対局がゼロ件で安定するまで待機する経路。
[Issue #601](https://github.com/SH11235/rshogi/issues/601) で配線済。staging は canary 用途 (`main` push 自動 deploy) のため
drain 対象外。

### 12.1 設計

- **Best-effort drain**: drain 観測完了 (= 例えば `live_games.length == 0` を 3
  回連続観測) の **後** に `wrangler deploy` upload が始まるまでの数秒間に新規
  `start_match` が走った場合、その対局は deploy 中の DO version cutover を踏む
  (= 対局が壊れる可能性が残る)。Workers は traffic gate を提供しないため、本経路
  では race window の完全排除はできない。「graceful drain」ではなく
  **"best-effort drain"** として doc / log で表記する。
- 真の graceful drain (進行中対局を新 isolate に migrate) は Workers 制約上
  実装不能。将来対局頻度が上がったら maintenance mode flag 経由の二段 deploy
  (lobby が `start_match` を拒否する flag を入れて pre-deploy → drain →
  本 deploy) を追加検討する。
- 最低 drain 検出時間 = `poll_interval_sec × require_stable_zero` (default 設定
  では `30s × 3 = 90s`)。R2 list の eventual consistency を緩和するためのバッファ。
- worst case 待機: `total_time_sec × 2 + byoyomi_sec × max_moves`
  (production CLOCK 設定で `600 × 2 + 10 × 256 = 3760s ≈ 63 分`) + 余裕で 3900s
  を default に設定。

### 12.2 ローカル手動 deploy 経路

```bash
LIVE_URL="https://rshogi-csa-server-workers.sh11235.workers.dev/api/v1/games/live"
bash scripts/check-csa-drain.sh \
  --live-url "$LIVE_URL" \
  --max-wait-sec 3900 \
  --poll-interval-sec 30 \
  --require-stable-zero 3 \
  && (cd crates/rshogi-csa-server-workers && pnpm wrangler deploy --config wrangler.production.toml)
```

script の引数 / 終了コード仕様は `scripts/check-csa-drain.sh --help` 参照。
exit 0 = drained, 1 = timeout, 2 = fetch error, 3 = usage error。

### 12.3 CI workflow_dispatch 経路

`gh workflow run deploy-workers.yml -f target=production` で起動。`production`
job が `actions/checkout@v5` 後に `Drain in-flight games (production)` step
を実行する。

#### 関連 inputs

| input | type | default | 用途 |
|---|---|---|---|
| `target` | choice | `staging` | `production` 選択で本経路 |
| `force_deploy` | boolean | `false` | drain step を bypass (緊急 hotfix 用) |
| `force_deploy_reason` | string | `''` | `force_deploy=true` 時に必須。空 reject |
| `drain_max_wait_sec` | number | `3900` | drain 待機上限 (≈65分) を per-deploy override |

#### 必須 environment variable

GitHub Environment `rshogi-csa-server-workers-production` の variables に
`WORKERS_DRAIN_URL` を登録 (例: `https://rshogi-csa-server-workers.sh11235.workers.dev/api/v1/games/live`)。
未設定で production deploy を起動すると drain step が `::error::` で fail する
(staging job は drain step を持たないため変数も不要)。

#### `force_deploy` 利用時

- `force_deploy=true` && `force_deploy_reason=''` で起動すると `Validate
  force_deploy reason` step が早期 fail。billed minutes を浪費しない。
- 通過時は Step Summary 1 行目に `> ⚠️ FORCE DEPLOY (drain bypassed): <reason>`
  banner が表示され、`::warning::` で workflow log にも reason が残る。
- 月次で振り返りやすくするため、reason には issue / incident 番号を含めるのが望ましい。

### 12.4 trouble shooting

#### drain が `drain_max_wait_sec` まで完了しない (exit 1)

1. live API を直接 curl して居残っている対局を確認:
   ```bash
   curl -s "$LIVE_URL" | jq '.live_games[] | {game_id, started_at_ms}'
   ```
2. game_id ごとに残対局時間を見積もる (`started_at_ms` + worst case 63 分 が deadline)。
3. 真に対局中なら deadline まで待つ。stuck (AGREE 待ち TTL 未配線、[Issue #600](https://github.com/SH11235/rshogi/issues/600) 対象)
   と判断したら DO 個別 reset 手順 (本節は別 issue で配線予定) または
   `force_deploy=true` + `force_deploy_reason="stuck DO during deploy drain"` で先に進める。

#### drain step が fetch error (exit 2) で fail

- `WORKERS_DRAIN_URL` の URL を `curl` で直接叩き、5xx か network 到達不能か判別。
- viewer API 自体が壊れている場合は live API 復旧を優先 (drain も観測手段として
  使えなくなる)。緊急性が高ければ `force_deploy=true` で先に deploy して、別途
  observability を立て直す。

#### drain step に query string 警告 (exit 3)

- `WORKERS_DRAIN_URL` がフルパスで `?cursor=...` 等を含んでいないか確認。script
  は path 末尾 (`/api/v1/games/live`) を渡す前提で、cursor 等の query string は
  内部で append する。

### 12.5 concurrency 注意

- production への `workflow_dispatch` を drain 中 (例: 50 分経過済) に追加で
  叩くと、`concurrency.group: deploy-workers-production` + `cancel-in-progress:
  false` の組合せで後続 dispatch は **queue されて累積待機** になる
  (= 65 分 × 2 dispatch で最大 130 分先まで待つ)。
- 緊急 hotfix が必要な場合は drain 中の dispatch を一旦キャンセル
  (`gh run cancel <run-id>`) してから `force_deploy=true` で再 dispatch するのが
  早い。

### 12.6 staging が drain 対象外な理由

- staging は `main` push 自動 deploy + 手動 dispatch の canary 用途。本番対局を
  載せない設計。
- もし staging に drain を入れると毎 PR merge ごとに最大 63 分 main push が
  block され、CI feedback loop が壊れる。
- staging の役割と矛盾するため `deploy-staging` job には drain step を入れず、
  `WORKERS_DRAIN_URL` も staging Environment には登録しない。

## 14. R2 backup / lifecycle / 棋譜永久保持 (#624)

棋譜は Floodgate 相当 production の **絶対消失防止対象**。`bucket.delete()` の
誤操作 / `%%DELETEBUOY` の暴走 / Cloudflare account 障害 / lifecycle rule
誤適用 のいずれでも棋譜が消えないよう、複数層の防御を組む。

### 14.1 R2 prefix taxonomy と削除ポリシー

production の `KIFU_BUCKET` 配下で書き込まれる prefix と、許可される delete
経路を以下に固定する。

| prefix | 内容 | 想定 writer | 削除ポリシー |
|---|---|---|---|
| `<YYYY>/<MM>/<DD>/<game_id>.csa` | 日付別 棋譜本体 | `game_room::export_kifu_to_r2` (`finalize_if_ended` 経路) | **永久保持** (delete なし、lifecycle rule で保護) |
| `kifu-by-id/<game_id>.csa` | game_id 逆引き 棋譜本体 | 同上 | **永久保持** (同上) |
| `kifu-by-id/<game_id>.meta.json` | 棋譜メタ JSON | 同上 + `backfill` (meta repair) | **永久保持** (同上) |
| `live-games-index/<inv>-<id>.json` | viewer 配信用 進行中 index | `mark_play_started` / cron sweep | 終局/orphan 検出時に DO + sweep が delete 可 |
| `games-index/<YYYY>/<MM>/<DD>/<id>.json` | viewer 配信用 終局済 index | 終局時 export | **永久保持** (lifecycle rule で保護) |
| `buoys/<encoded>.json` | %%SETBUOY 登録 | `store_buoy` | `%%DELETEBUOY` (admin 限定) で delete 可 |

**code 側の delete 経路** は本 doc 時点で 3 箇所のみ:

1. `game_room::delete_buoy` — `buoys/` prefix のみ。`x1_paths::BUOY_KEY_PREFIX`
   で runtime 検証 (#624 隣接懸念)。
2. `game_room::try_delete_live_games_index` — `live-games-index/` のみ。
   key は `live_games_index::live_index_object_key()` 経由で生成。
3. `backfill::sweep_orphan_live_index` — 同 `live-games-index/` のみ。

**棋譜本体 (`<YYYY>/...`, `kifu-by-id/...`, `games-index/...`) を delete する
コードは存在しない**。レビュー時はこの不変条件を維持すること。

### 14.2 R2 lifecycle rule 設計 (Pulumi IaC で declarative 化)

`rshogi-cloudflare-iac` repo (PR #677 で R2 buckets を IaC 化済) に lifecycle
policy を declarative に追記する。本 repo 側は **wrangler.toml では設定せず**、
IaC 側を single source of truth とする (drift detection で gate 済)。

設計:

```
{KIFU_BUCKET}:
  rules:
    - name: abort-incomplete-multipart
      match: { prefix: "" }
      abort_incomplete_multipart_upload:
        days_after_initiation: 7
    # 棋譜 prefix は明示的に expire しない (永久保持)。
    # lifecycle rule で `delete` action を一切付与しないことで R2 側の自動削除
    # 経路を完全に塞ぐ。誤って rule が増えた場合は drift detection で検出。
```

**注意**:
- Cloudflare R2 lifecycle rule は現時点で「expire」「abort multipart」をサポート。
  S3 互換の `noncurrent` 系は未対応 (versioning 自体が R2 native では未提供)。
- `delete` action のある rule を追加する場合は **必ず** 棋譜 prefix を含まない
  ことを Pulumi 側 unit test (rshogi-cloudflare-iac) でガードする。

### 14.3 別 backup bucket への定期 cp cron (Phase 2)

本 PR では設計のみ。実装は #624 Phase 2 別 PR で扱う。

- backup bucket 名: `rshogi-csa-kifu-backup` (production), `…-staging-backup` (staging)
- 同 account / 別 bucket。Cloudflare account 障害には弱いが、誤 delete /
  bucket 誤操作からは防御できる
- Workers `scheduled` event で 1 日 1 回 (UTC 03:00) 実行
- 前日の `<YYYY>/<MM>/<DD>/...` prefix を `KIFU_BUCKET.list` で列挙、
  `BACKUP_BUCKET.put(key, body)` でコピー (上書き protection: HEAD で existence 確認後 skip)
- `kifu-by-id/` は 1 日分の差分 list 困難のため、終局時の export 経路で
  primary + backup の 2-bucket 同時 PUT を将来検討
- ops budget: Class A 20 ops/日 = 600 ops/月 (Free 1M の 0.06%)

### 14.4 restore runbook

#### 14.4.1 単一棋譜の誤削除を疑った場合

1. **R2 console** で `<YYYY>/<MM>/<DD>/<game_id>.csa` の存在を確認
2. 不在なら backup bucket (Phase 2 配線後) から download:
   ```bash
   vp exec wrangler r2 object get "rshogi-csa-kifu-backup/<YYYY>/<MM>/<DD>/<game_id>.csa" --file=/tmp/restored.csa
   ```
3. primary bucket へ put し直す:
   ```bash
   vp exec wrangler r2 object put "rshogi-csa-kifu/<YYYY>/<MM>/<DD>/<game_id>.csa" --file=/tmp/restored.csa
   ```
4. `kifu-by-id/<game_id>.csa` と `<game_id>.meta.json` も同様に確認・復旧
5. `games-index/<YYYY>/<MM>/<DD>/<id>.json` がある場合のみ viewer に表示される
   ため、必要なら `backfill` 経路 (admin endpoint) でインデックス再生成

#### 14.4.2 大規模消失 / Cloudflare account 障害

1. backup bucket の最新 snapshot 時刻を `wrangler r2 object list` で確認
2. 喪失範囲を date prefix 単位で特定 (`<YYYY>/<MM>/<DD>` 別)
3. backup bucket → primary bucket への一括 cp を Workers cron 手動 invoke
   (`wrangler cron trigger backup-restore` で対応 trigger を起動) で復旧
4. cp 完了後に `backfill` で `kifu-by-id/` と `games-index/` を再生成
5. インシデント記録を作成 (`docs/csa-server/incidents/<YYYY-MM-DD>.md` 参照、
   §13.5 の DO 喪失 incident テンプレに準拠)

#### 14.4.3 backup bucket 自体が壊れた場合

primary が生きている前提なら、primary → backup の再 sync で復旧。両方喪失の
場合は Cloudflare サポートへエスカレーション (point-in-time restore は
Cloudflare 側機能に依存)。

### 14.5 follow-up

- [ ] `rshogi-cloudflare-iac` で R2 lifecycle policy を Pulumi に追加 (Phase 1
      doc 公開と同時に着手)
- [ ] backup bucket への cp cron 実装 (#624 Phase 2)
- [ ] viewer 経路で `games-index/` の bucket 整合性 doctor (Phase 2 と同時着手)
