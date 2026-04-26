# rshogi-csa-server-workers デプロイ運用 Runbook

Cloudflare Workers 上で `rshogi-csa-server-workers` を本番運用するための
セットアップと運用手順をまとめる。**初回構築時** は §1〜§3 を順に実行する。
**通常運用** で参照するのは §4〜§6。

## 0. アーキテクチャ概要

```
┌──────────────────────────────────────────────────────────────────┐
│ Cloudflare edge                                                  │
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

- 1 ルーム = 1 Durable Object instance（`id_from_name(room_id)` で決定論解決）
- WebSocket Hibernation を使い、対局アイドル中は worker を停止状態で保持
- 棋譜は R2 (`KIFU_BUCKET`)、Floodgate 履歴は R2 (`FLOODGATE_HISTORY_BUCKET`)
  にそれぞれ書き出す
- Alarm API で時間切れを検知して終局確定

## 1. 必要なもの

| 区分 | 要件 |
|---|---|
| Cloudflare アカウント | **Workers Free プランで本構成は動作**（詳細は §1.1）。本格運用で Free 制限を超える見込みなら Workers Paid を検討 |
| GitHub リポジトリ権限 | Settings → Secrets and variables への書き込み（Admin 相当） |
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

以降 `wrangler` は `vp exec wrangler ...` または scripts (`vp run deploy:prod`
等) 経由で呼び出す。`npm install -g wrangler` / `pnpm add -g wrangler` などの
global install は不要。

### プラン確認（Free でも動作する）

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

[Workers & Pages → Plans](https://dash.cloudflare.com/?to=/:account/workers/plans)
で現プランを確認可能。CLI からは §3.2 で `vp exec wrangler login` を済ませた後で:

```bash
vp exec wrangler whoami
```

を打つと現在認証中のアカウント情報が出る。

## 2. 初回 Cloudflare セットアップ

### 2.1 R2 buckets を作成

```bash
cd crates/rshogi-csa-server-workers
vp exec wrangler r2 bucket create rshogi-csa-kifu-prod
vp exec wrangler r2 bucket create rshogi-csa-floodgate-history-prod
```

bucket 名はリポジトリ管理の `crates/rshogi-csa-server-workers/wrangler.production.toml`
の `bucket_name` と一致させる。命名規約は Cloudflare 側の制約（小文字英数字 +
ハイフン、3〜63 文字、先頭末尾は英数字）に従う。

### 2.2 API token を作成

[Cloudflare Dashboard → My Profile → API Tokens](https://dash.cloudflare.com/profile/api-tokens)
から **Create Token**。

**推奨**: テンプレートから "Cloudflare Workers を編集する" (英語版: "Edit
Cloudflare Workers") を選択する。これだけで Workers Scripts / Workers KV /
Workers R2 / Workers Tail / membership read など実運用に十分な権限が一括
付与される。Account Resources は本リポジトリ用のアカウントのみに絞ること。

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
| Account | Workers Tail | Read （`vp tail:prod` で必要）|
| Account | Account Settings | Read |
| User | Memberships | Read |

> ⚠️ 作成した token は **画面遷移すると 2 度と表示されない**。そのまま手元の
> パスワードマネージャ等に控えてから次へ進む。

### 2.3 Account ID を取得

[Cloudflare Dashboard](https://dash.cloudflare.com/) 右下の "Account details"
セクション → "Account ID" をコピー。

### 2.4 GitHub Secrets を設定

リポジトリ → Settings → Secrets and variables → Actions → New repository secret:

| Name | 値 |
|---|---|
| `CLOUDFLARE_API_TOKEN` | §2.2 で作成した token |
| `CLOUDFLARE_ACCOUNT_ID` | §2.3 の Account ID |

設定すると `.github/workflows/deploy-workers.yml` の preflight job が
`can_deploy=true` になり、以降 `main` への push で自動 deploy が起動する。

### 2.4.5 Cloudflare 上で Worker secret を設定する

`wrangler.production.toml` の `[vars]` には書かない値を Cloudflare 側で
secret として登録する。本リポジトリで現状必要な secret:

| Secret | 用途 |
|---|---|
| `ADMIN_HANDLE` | `%%SETBUOY` / `%%DELETEBUOY` を許可する運営ハンドル名。OSS repo に handle 名が出ない経路で defense-in-depth を保つ |

設定手順（§3.2 で `vp exec wrangler login` を済ませた後で）:

```bash
cd crates/rshogi-csa-server-workers
vp exec wrangler secret put ADMIN_HANDLE --config wrangler.production.toml
# プロンプトに値を入力 (echo されない)。例: "rshogi-ops" など、
# 一般的でない文字列を選ぶ
```

> ℹ️ secret の値は Cloudflare 側で encrypted at rest され、CI ログにも commit
> 履歴にも残らない。Worker code は `[vars]` と同じ namespace から
> `env.var(ConfigKeys::ADMIN_HANDLE)` で読む（Cloudflare 仕様）。
>
> rotation したい場合は同じコマンドを再度実行すれば上書きされる。
> 削除は `vp exec wrangler secret delete ADMIN_HANDLE --config wrangler.production.toml`。
>
> 設定済みの secret 一覧は
> `vp exec wrangler secret list --config wrangler.production.toml` で確認。

### 2.5 (オプション) Health URL を repository variable に設定

[同画面 → Variables tab → New repository variable]:

| Name | 値（例） |
|---|---|
| `WORKERS_HEALTH_URL` | `https://rshogi-csa-server-workers.<your-subdomain>.workers.dev/health` |

`<your-subdomain>` は Cloudflare アカウント固有の workers.dev サブドメイン
（例: `your-name`）。**§3.3 の `wrangler deploy` 実行ログ末尾に
`Published rshogi-csa-server-workers (X.YZs) https://...workers.dev` という形で
完全な URL が出力される**ので、その値を流用する。
[Cloudflare Dashboard → Workers & Pages → `rshogi-csa-server-workers` →
Triggers → Routes] でも確認できる。

これを設定すると、deploy 完了後に CI が `/health` を curl で叩いて smoke check
する step が起動する。値未設定なら smoke step は skip されるだけで deploy 自体は
成功扱い。**§3.3 の初回 deploy が成功してから設定する** こと。

## 3. 初回手動 deploy

CI 自動 deploy 前に、ローカルから 1 度手動で deploy して動作確認する。
**この手順は最初の 1 度だけ**。以降は CI が自動で deploy する。

> 🛑 **§3.2 以降に進む前に、§3.1 の値設定 PR を必ず merge しておく**こと。
> 特に `CORS_ORIGINS` を空のまま deploy すると、`/health` は応答するものの
> 全 WS Upgrade が 403 で拒否される（client が一切繋がらない状態）に陥り、
> 運用者を混乱させる。値を埋めた PR を main に通してから §3.2 の手動 deploy に
> 進むこと。

### 3.1 wrangler.production.toml の値を確定する

`crates/rshogi-csa-server-workers/wrangler.production.toml` を開き、本番値に
編集する PR を作って main に merge する:

- `[[r2_buckets]] bucket_name` が §2.1 で作成した bucket 名と一致しているか
- `[vars] CORS_ORIGINS` に本番 client の Origin（例:
  `https://rshogi.example.com,https://www.rshogi.example.com`）が入っているか
  — 空のままだと全 Upgrade を 403 で拒否する（安全側既定）
- 時計設定（`CLOCK_KIND` / `TOTAL_TIME_*` / `BYOYOMI_*`）を運用方針に合わせる
- ⚠️ **`ADMIN_HANDLE` を Cloudflare secret に設定済みか確認する**（§2.4.5）。
  未設定だと `%%SETBUOY` / `%%DELETEBUOY` が誰の入力にも一致せず通らない。
  確認: `cd crates/rshogi-csa-server-workers && vp exec wrangler secret list --config wrangler.production.toml`

> ℹ️ `ADMIN_HANDLE` は本ファイルには **書かない**。§2.4.5 で説明した通り
> Cloudflare secret として `wrangler secret put` 経由で設定する。
> 整合性 test (`tests/wrangler_production_toml_consistency.rs`) が
> `wrangler.production.toml` の `[vars]` に `ADMIN_HANDLE` が混入していたら
> CI で fail させる契約。

### 3.2 wrangler login

```bash
cd crates/rshogi-csa-server-workers
vp exec wrangler login
```

ブラウザが開いて Cloudflare OAuth 認証 → 完了後ターミナルに戻る。

### 3.3 deploy（ビルド込み）

```bash
# crates/rshogi-csa-server-workers にいる前提
vp run deploy:prod
```

`vp run deploy:prod` は `package.json` で
`wrangler deploy --config wrangler.production.toml` のショートカット。
`wrangler.production.toml` の `[build] command = "worker-build --release"` が
`wrangler deploy` の前段で自動実行されるため、**個別の `worker-build`
事前ビルドは不要**。`build/worker/shim.mjs` と `build/index_bg.wasm` が
生成された後、Cloudflare に upload される。

成功すると `https://rshogi-csa-server-workers.<your-subdomain>.workers.dev/`
にデプロイされ、URL が標準出力に表示される。

### 3.4 Smoke check

```bash
SUBDOMAIN=<your-subdomain>
curl "https://rshogi-csa-server-workers.${SUBDOMAIN}.workers.dev/health"
# → "rshogi-csa-server-workers v0.1.0"
```

WebSocket 疎通は別途 wsclient ツールで確認:

```bash
# `websocat` が無い場合は `cargo install websocat` か `brew install websocat` で
# 入れる。`wscat` (`npm i -g wscat`) や任意の WS client でも代用可。
websocat "wss://rshogi-csa-server-workers.${SUBDOMAIN}.workers.dev/ws/test-room-1" \
  -H "Origin: https://rshogi.example.com"
# 接続が確立すれば OK（"LOGIN ..." を入力すると Worker 側で受理する）
```

成功したら §2.5 で `WORKERS_HEALTH_URL` を repository variable に登録し、
以降 CI が deploy 後に自動 smoke check を行う。

### 3.5 DO migration 確認

`wrangler deployments list` は migration tag を表示しない。確認手段は 2 つ:

**(a) 初回 deploy ログを見る**

§3.3 の `wrangler deploy` 実行ログに以下のような行が出ていれば apply 済み:

```
- new_sqlite_classes: GameRoom
```

**(b) Cloudflare Dashboard で確認**

[Workers & Pages → `rshogi-csa-server-workers` → Settings → Durable Objects]
セクションで `GameRoom` クラスが SQLite-backed として表示されていれば apply 済み。

DO migration は **同 tag を再 apply しても skip される**ので、`tag = "v1"` の
内容を変更する場合は新 tag (`v2` / `v3` ...) を `wrangler.production.toml` に
**追加** する。既存 tag を編集しても無視される。

## 4. 通常運用（自動 deploy）

### 4.1 deploy フロー

```
PR 作成 → CI (rust-ci.yml) で fmt/lint/test/wasm-build 全 pass
       ↓
PR merge to main
       ↓
deploy-workers.yml が起動
       ↓
preflight: secrets チェック
       ↓
deploy: wrangler-action で wrangler deploy
       ↓
smoke: /health を curl（WORKERS_HEALTH_URL 設定時のみ）
       ↓
Cloudflare に新版が反映（数秒〜数十秒で全エッジに rollout）
```

deploy が trigger される path（`.github/workflows/deploy-workers.yml` 参照）:

- `crates/rshogi-csa-server-workers/**`
- `crates/rshogi-csa-server/**`
- `crates/rshogi-csa/**`
- `crates/rshogi-core/**`
- `Cargo.toml` / `Cargo.lock`
- `.github/workflows/deploy-workers.yml`

これら以外（docs / TCP only crate / 他 workspace member）の変更では deploy は
起動しない。

### 4.2 手動 deploy

緊急時 / rollback 後の再 apply 等で手動起動したい場合:

[GitHub → Actions → Deploy Workers → Run workflow → main → Run]

または CLI から:

```bash
gh workflow run deploy-workers.yml --ref main
```

## 5. Rollback

### 5.1 直前 version に戻す

```bash
cd crates/rshogi-csa-server-workers
vp exec wrangler rollback --config wrangler.production.toml
```

確認 prompt が出るので `y` で確定。Cloudflare 側で前 version に切り替わる
（数秒で反映）。

### 5.2 特定 version に戻す

```bash
vp exec wrangler deployments list --config wrangler.production.toml
# Version ID をコピー

vp exec wrangler rollback <version-id> --config wrangler.production.toml
```

### 5.3 Rollback 後の repo state 同期

`wrangler rollback` は Cloudflare 側だけを巻き戻す。リポジトリの code は
そのまま。**rollback で対応した不具合の本修正 PR を必ず追って main に出し、
通常 deploy で前進する**。rollback したまま放置すると次の自動 deploy で
壊れたコードが再度 apply される。

### 5.4 自動 deploy job が途中で失敗したとき

CI 上の deploy job が失敗 (`wrangler-action` が non-zero) した場合、Cloudflare
側の状態は **失敗時点まで進んでいる可能性** がある（一部 binding 更新だけが
反映された等）。以下の順で復旧する:

1. **失敗ログを確認**
   - GitHub Actions の job log を一読し `Error:` 行で原因を切り分ける
   - `Authentication error (10000)`: §6.3 の token 系
   - `R2 bucket not found`: §6.3 の bucket 系
   - `Migration tag conflict`: §3.5 の migration 同 tag 再 apply（変更が apply
     されたかどうかを Dashboard で確認、必要なら新 tag で出し直す）

2. **Cloudflare 側の現在 version を確認**
   ```bash
   cd crates/rshogi-csa-server-workers
   vp exec wrangler deployments list --config wrangler.production.toml
   ```
   最新 version が deploy job 開始 **前** のものなら未反映 → 再実行で OK。
   deploy job 中の途中 version になっていたら次へ。

3. **不整合状態の場合は §5.1 / §5.2 の手順で安定 version に rollback**
   - 直前の安定 version に戻すなら §5.1（`vp exec wrangler rollback --config ...`）
   - 特定の安定 version に戻すなら §5.2（`vp exec wrangler deployments list` で ID を
     確認して `vp exec wrangler rollback <version-id> --config ...`）
   - rollback 後は §5.3 の通り、修正を main に戻す PR を必ず追って出す

4. **修正 PR or 同 commit の workflow 再実行**
   - 設定値の問題 (token / secrets / toml) なら修正 PR を main に出して通常 flow
   - 一過性 (network / Cloudflare 側の障害) なら `gh workflow run deploy-workers.yml --ref main`
     で同 commit の deploy を再試行

5. **wrangler tail でクライアント影響を観察**
   ```bash
   vp run tail:prod
   ```
   既存接続が切れていないか / 新規接続が成立しているかを確認してから運用復帰。

> 💡 deploy job は `concurrency: deploy-workers / cancel-in-progress: false` で
> 同時実行が serialize される。失敗 job を放置して次の merge を進めると、新 push
> の deploy が後ろで待つ。失敗の追加調査が必要なら GitHub Actions 画面で当該
> deploy job を **手動 cancel** してから次に進めること。

## 6. 監視 / トラブルシューティング

### 6.1 ログを見る

```bash
cd crates/rshogi-csa-server-workers
vp run tail:prod
```

`vp run tail:prod` は `package.json` で
`wrangler tail --config wrangler.production.toml` のショートカット。
`console_log!` 出力 + 例外が realtime で流れる。終局後の R2 PUT 失敗等は
ここで観測できる。

> ⚠️ 構造化ログは task 17.7 で本格整備予定。現状は文字列ログのみ。

### 6.2 メトリクスを見る

[Cloudflare Dashboard → Workers & Pages → `rshogi-csa-server-workers` → Metrics]

Requests / errors / CPU time / WS connections 数等。詳細指標 (P99 レイテンシ
等) は task 20.2 で導入予定。

### 6.3 よくある問題

#### deploy 失敗: `R2 bucket not found`

- `wrangler.production.toml` の `bucket_name` が誤っている、または bucket が
  Cloudflare 上に未作成
- 対処: §2.1 の `vp exec wrangler r2 bucket create` を実行、または toml 側を修正

#### deploy 失敗: `Authentication error (10000)`

- `CLOUDFLARE_API_TOKEN` の permission 不足
- 対処: §2.2 で token を再作成し permissions を確認、GitHub Secrets を更新

#### deploy 成功するが対局できない

- DO migration が apply されていない（最初の deploy で必ず apply される）
- `[vars] CORS_ORIGINS` が空 or 誤った Origin → WS Upgrade が 403
- 対処: §6.1 の `vp run tail:prod` で wrangler tail を見ながら client から接続して
  4xx/5xx を観測

#### Hibernation が効かない

- `state.accept_web_socket()` 経由でなく標準 WS API を使っているケース
  （現実装では使っていないので発生しない想定）
- DO instance が active connection を持ち続けると Hibernation には入らない
  → 設計通りの挙動

#### 自動 deploy が起動しない

- preflight job が `can_deploy=false` で skip している → §2.4 の secrets を確認
- push の path filter に該当しない → §4.1 の path リストを確認

### 6.4 関連 task

| Task | 内容 |
|---|---|
| 23.1 | CI に wasm32 ビルド検査 job を追加（PR #503） |
| 23.2 | wrangler.toml.example と ConfigKeys 整合性（PR #505）|
| 23.3 | Miniflare smoke E2E ハーネス（未着手）|
| 23.6 | `ADMIN_HANDLE` を Cloudflare secret に格上げ + production toml 整合性 test |
| 17.7 | Workers structured logging（本ドキュメント §6.1 のログ整備）|
| 20.2 | Workers 負荷試験ハーネス + 詳細メトリクス（本ドキュメント §6.2 のメトリクス整備）|

## 7. 設定ファイル比較

| File | 用途 | 管理 | `[vars]` で持つ値 |
|---|---|---|---|
| `wrangler.toml.example` | local 開発・新規メンバー向け template | Tracked | 公開値（`PRODUCTION_VARS_KEYS`）+ local 専用 placeholder（`LOCAL_DEV_ONLY_VARS_KEYS`、例: `ADMIN_HANDLE`）|
| `wrangler.toml` | 各開発者の local 個人設定 | Gitignored | `.example` 由来 |
| `wrangler.production.toml` | CI 自動 deploy 用本番設定 | **Tracked**（本 doc §3.1 参照）| 公開値（`PRODUCTION_VARS_KEYS`）のみ。`LOCAL_DEV_ONLY_VARS_KEYS` は **書かない**（§2.4.5 で secret 化） |

`wrangler.production.toml` を tracked にしている理由は、bucket 名 / `CORS_ORIGINS` /
時計設定など **機密でないがインフラ仕様として固定したい値** を全員で共有
するため。秘匿情報（API token / account_id / `ADMIN_HANDLE`）は GitHub Secrets
（CI 認証用）または Cloudflare Worker secret（runtime 用）経由で注入し、
本ファイルに直接書かない。

整合性 test:
- `tests/wrangler_template_consistency.rs`: `wrangler.toml.example` の `[vars]` が
  `PRODUCTION_VARS_KEYS ∪ LOCAL_DEV_ONLY_VARS_KEYS` と双方向に一致することを assert
- `tests/wrangler_production_toml_consistency.rs`: `wrangler.production.toml` の
  `[vars]` が `PRODUCTION_VARS_KEYS` 単独と一致し、かつ `LOCAL_DEV_ONLY_VARS_KEYS` の
  各キーが含まれていないことを assert（secret 経路の前提を gate）
