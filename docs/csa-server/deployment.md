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
| Cloudflare アカウント | **Workers Paid プラン**（Durable Objects + R2 利用のため Free では不可） |
| GitHub リポジトリ権限 | Settings → Secrets and variables への書き込み（Admin 相当） |
| ローカル環境 | Node.js 20+ / `wrangler` CLI / Rust toolchain + `wasm32-unknown-unknown` target |

```bash
# 必要に応じて
rustup target add wasm32-unknown-unknown
npm install -g wrangler
# CI deploy workflow と同じバージョン pin に揃える。新しい minor が出ても
# 自動追従するので運用者側で都度 update する必要はない。
cargo install -q worker-build@^0.8 --locked
```

### Workers Paid プランの確認

Cloudflare Dashboard で先に確認しておく:

[Workers & Pages → Plans](https://dash.cloudflare.com/?to=/:account/workers/plans)
を開き、現プランが **Workers Paid** であることを確認する。Free プランの場合は
ここでアップグレードしてから §2 以降に進む。Free のまま §3 の deploy を実行
すると Durable Objects / R2 関連の権限エラーで停止する。

CLI からも確認したい場合は §3.2 で `wrangler login` を済ませた後で:

```bash
wrangler whoami
```

を打つと現在認証中のアカウント情報が出る（プラン情報の表示は wrangler の
バージョンによる。確実なのは Dashboard 側の確認）。

## 2. 初回 Cloudflare セットアップ

### 2.1 R2 buckets を作成

```bash
wrangler r2 bucket create rshogi-csa-kifu-prod
wrangler r2 bucket create rshogi-csa-floodgate-history-prod
```

bucket 名はリポジトリ管理の `crates/rshogi-csa-server-workers/wrangler.production.toml`
の `bucket_name` と一致させる。命名規約は Cloudflare 側の制約（小文字英数字 +
ハイフン、3〜63 文字、先頭末尾は英数字）に従う。

### 2.2 API token を作成

[Cloudflare Dashboard → My Profile → API Tokens](https://dash.cloudflare.com/profile/api-tokens)
から **Create Token**。

**推奨**: テンプレートから "Edit Cloudflare Workers" を選択する。これだけで
Workers Scripts / Workers KV / Workers R2 / Durable Objects / 必要な
membership read など実運用に十分な権限が一括付与される。Account Resources は
本リポジトリ用のアカウントのみに絞ること。

> ℹ️ preset の権限内訳は Cloudflare 側仕様で更新されることがあるため、token 作成
> 直前のサマリ画面で `Workers Scripts:Edit` / `Workers R2 Storage:Edit` /
> `Durable Objects:Edit` の 3 つが含まれていることを確認してから "Create Token"
> を押す。preset から脱落していた場合は "Custom token" に切り替えて下表を反映する。

詳細を絞りたい場合は "Custom token" で以下を組み合わせる:

| Scope | Resource | Permission |
|---|---|---|
| Account | Workers Scripts | Edit |
| Account | Workers R2 Storage | Edit |
| Account | Durable Objects | Edit |
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

### 2.5 (オプション) Health URL を repository variable に設定

[同画面 → Variables tab → New repository variable]:

| Name | 値（例） |
|---|---|
| `WORKERS_HEALTH_URL` | `https://rshogi-csa-server-workers.<your-subdomain>.workers.dev/health` |

`<your-subdomain>` は Cloudflare アカウント固有の workers.dev サブドメイン
（例: `your-name`）。**§3.4 の `wrangler deploy` 実行ログ末尾に
`Published rshogi-csa-server-workers (X.YZs) https://...workers.dev` という形で
完全な URL が出力される**ので、その値を流用する。
[Cloudflare Dashboard → Workers & Pages → `rshogi-csa-server-workers` →
Triggers → Routes] でも確認できる。

これを設定すると、deploy 完了後に CI が `/health` を curl で叩いて smoke check
する step が起動する。値未設定なら smoke step は skip されるだけで deploy 自体は
成功扱い。**§3.4 の初回 deploy が成功してから設定する** こと。

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
- `[vars] ADMIN_HANDLE` を運用上の admin handle に変更したい場合（任意）

### 3.2 wrangler login

```bash
wrangler login
```

ブラウザが開いて Cloudflare OAuth 認証 → 完了後ターミナルに戻る。

### 3.3 ビルド

```bash
cd crates/rshogi-csa-server-workers
worker-build --release
```

`build/worker/shim.mjs` と `build/index_bg.wasm` が生成される。

### 3.4 deploy

```bash
wrangler deploy --config wrangler.production.toml
```

成功すると `https://rshogi-csa-server-workers.<your-subdomain>.workers.dev/`
にデプロイされ、URL が標準出力に表示される。

### 3.5 Smoke check

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

### 3.6 DO migration 確認

`wrangler deployments list` は migration tag を表示しない。確認手段は 2 つ:

**(a) 初回 deploy ログを見る**

§3.4 の `wrangler deploy` 実行ログに以下のような行が出ていれば apply 済み:

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
wrangler rollback --config wrangler.production.toml
```

確認 prompt が出るので `y` で確定。Cloudflare 側で前 version に切り替わる
（数秒で反映）。

### 5.2 特定 version に戻す

```bash
wrangler deployments list --config wrangler.production.toml
# Version ID をコピー

wrangler rollback <version-id> --config wrangler.production.toml
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
   - `Migration tag conflict`: §3.6 の migration 同 tag 再 apply（変更が apply
     されたかどうかを Dashboard で確認、必要なら新 tag で出し直す）

2. **Cloudflare 側の現在 version を確認**
   ```bash
   wrangler deployments list --config crates/rshogi-csa-server-workers/wrangler.production.toml
   ```
   最新 version が deploy job 開始 **前** のものなら未反映 → 再実行で OK。
   deploy job 中の途中 version になっていたら次へ。

3. **不整合状態の場合は §5.1 / §5.2 の手順で安定 version に rollback**
   - 直前の安定 version に戻すなら §5.1（`wrangler rollback --config ...`）
   - 特定の安定 version に戻すなら §5.2（`wrangler deployments list` で ID を
     確認して `wrangler rollback <version-id> --config ...`）
   - rollback 後は §5.3 の通り、修正を main に戻す PR を必ず追って出す

4. **修正 PR or 同 commit の workflow 再実行**
   - 設定値の問題 (token / secrets / toml) なら修正 PR を main に出して通常 flow
   - 一過性 (network / Cloudflare 側の障害) なら `gh workflow run deploy-workers.yml --ref main`
     で同 commit の deploy を再試行

5. **wrangler tail でクライアント影響を観察**
   ```bash
   wrangler tail --config crates/rshogi-csa-server-workers/wrangler.production.toml
   ```
   既存接続が切れていないか / 新規接続が成立しているかを確認してから運用復帰。

> 💡 deploy job は `concurrency: deploy-workers / cancel-in-progress: false` で
> 同時実行が serialize される。失敗 job を放置して次の merge を進めると、新 push
> の deploy が後ろで待つ。失敗の追加調査が必要なら GitHub Actions 画面で当該
> deploy job を **手動 cancel** してから次に進めること。

## 6. 監視 / トラブルシューティング

### 6.1 ログを見る

```bash
wrangler tail --config crates/rshogi-csa-server-workers/wrangler.production.toml
```

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
- 対処: §2.1 の `wrangler r2 bucket create` を実行、または toml 側を修正

#### deploy 失敗: `Authentication error (10000)`

- `CLOUDFLARE_API_TOKEN` の permission 不足
- 対処: §2.2 で token を再作成し permissions を確認、GitHub Secrets を更新

#### deploy 成功するが対局できない

- DO migration が apply されていない（最初の deploy で必ず apply される）
- `[vars] CORS_ORIGINS` が空 or 誤った Origin → WS Upgrade が 403
- 対処: §6.1 で wrangler tail を見ながら client から接続して 4xx/5xx を観測

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
| 23.2 | wrangler.toml.example と ConfigKeys 整合性（PR #505、main に merge 後 follow-up あり）|
| 23.3 | Miniflare smoke E2E ハーネス（未着手）|
| 17.7 | Workers structured logging（本ドキュメント §6.1 のログ整備）|
| 20.2 | Workers 負荷試験ハーネス + 詳細メトリクス（本ドキュメント §6.2 のメトリクス整備）|

#### 6.4.1 Follow-up: `wrangler.production.toml` の整合性 test

PR #505 (task 23.2) で `wrangler.toml.example` ↔ `ConfigKeys::ALL_*` の双方向
整合 test を導入する。同パターンを `wrangler.production.toml` 側にも適用する
follow-up が必要（本 task 23.4 の PR 内では PR #505 が main に merge 前のため
`ConfigKeys::ALL_*` 定数を参照できず先送り）:

- `tests/wrangler_production_toml_consistency.rs` を新設
- 同じ双方向 assert ヘルパで `wrangler.production.toml` の binding/var を
  `ConfigKeys::ALL_*` と照合
- 追加で `[[migrations]] new_sqlite_classes = ["GameRoom"]` の存在も assert
  （本番固有の整合性）

PR #505 が main に merge され次第、別 PR で着手する。

## 7. 設定ファイル比較

| File | 用途 | 管理 |
|---|---|---|
| `wrangler.toml.example` | local 開発・新規メンバー向け template | Tracked |
| `wrangler.toml` | 各開発者の local 個人設定 | Gitignored |
| `wrangler.production.toml` | CI 自動 deploy 用本番設定 | **Tracked**（本 doc §3.1 参照）|

`wrangler.production.toml` を tracked にしている理由は、bucket 名 / `CORS_ORIGINS` /
時計設定など **機密でないがインフラ仕様として固定したい値** を全員で共有
するため。秘匿情報（API token / account_id）は GitHub Secrets 経由で env から
注入し、本ファイルに直接書かない。
