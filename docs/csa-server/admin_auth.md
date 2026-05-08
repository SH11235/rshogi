# admin 認可 (`ADMIN_API_TOKEN` + `%%ADMIN`) 運用ガイド

Floodgate audit ([#560](https://github.com/SH11235/rshogi/issues/560) +
[#621](https://github.com/SH11235/rshogi/issues/621)) で導入した admin 認可
基盤の運用手順。WS 内 admin command (`%%ADMIN <token>`) と将来の HTTP admin
endpoint が共通の `ADMIN_API_TOKEN` secret を踏む。

本ドキュメントは「token をどう作って Cloudflare に登録し、どう rotate するか」
の運用部分と「admin client が token をどう提示するか」のフローをカバーする。
コード側の検証ロジック仕様は
[`crate::admin_auth`](../../crates/rshogi-csa-server-workers/src/admin_auth.rs)
の docstring を参照。

## 旧 `ADMIN_HANDLE` からの移行 (Breaking change, 2026-05-09)

[#621](https://github.com/SH11235/rshogi/issues/621) で「LOGIN handle 自称 →
admin 権限付与」(handle equality 比較) の経路は廃止された。今後は **すべての
admin 権限要求コマンド (`%%SETBUOY` / `%%DELETEBUOY`) は同一 session 内で
事前に `%%ADMIN <token>` を通過していなければ `PERMISSION_DENIED` で拒否
される**。

運用 client の更新点 (CSA WS シーケンス):

```
LOGIN <handle>+<game_name>+<color> <password>
%%ADMIN <ADMIN_API_TOKEN>            ← 新規。session 内で 1 回踏めば良い
%%SETBUOY <game_name> <moves> <count>
...
```

`%%ADMIN <token>` 受信時の応答:

| ケース | 応答 |
|---|---|
| `verify_admin_token_str` 成功 (token 一致) | `##[ADMIN] OK` + `##[ADMIN] END` (session を admin 昇格) |
| token 不一致 / secret 未配置 | `##[ADMIN] PERMISSION_DENIED` + `##[ADMIN] END` (同一応答で「configured かどうか」を leak しない) |
| token 部欠落 (`%%ADMIN` 単体 / `%%ADMIN` の後に whitespace のみ) | **応答なし (silent ignore)**。`%%SETBUOY` 引数欠落等と同じく malformed command として握り潰す |

session が close した時点で admin 権限は失われる。

Cloudflare 側に旧 `ADMIN_HANDLE` secret が残っていれば、Worker code は
読まなくなったので運用上の混乱を避けるため削除推奨:

```bash
vp exec wrangler secret delete ADMIN_HANDLE --config wrangler.<env>.toml
```

## 1. 設計サマリ

| 項目 | 採用案 | 理由 |
|---|---|---|
| 認可方式 | static API token | replay 対策や canonical string 設計が必要な HMAC は overkill |
| 配置 | Cloudflare secret (`wrangler secret put ADMIN_API_TOKEN`) | OSS repo / CI ログ / `wrangler.<env>.toml` に値が残らない |
| 比較 | [`subtle::ConstantTimeEq`] による constant-time | timing leak で token の brute force 加速を防ぐ |
| Cloudflare Access | 別管理 (運用層) | コード変更なしで IP / SSO 制限を上乗せできる |
| 旧 token grace | 持たない (1 token のみ valid) | rotation の窓を最短化、bookkeeping を排除 |

[`subtle::ConstantTimeEq`]: https://docs.rs/subtle/latest/subtle/trait.ConstantTimeEq.html

## 2. token 生成

256bit (32 byte) 以上の URL-safe random を推奨。OSS repo に値が混入しない経路
で生成する。例 (どれを使ってもよい、いずれも 32 byte = 256bit のエントロピー):

```bash
# openssl (64 文字の hex、長さが固定で読みやすい)
openssl rand -hex 32

# openssl (URL-safe base64、'+' '/' を '-' '_' に置換、padding を除去)
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='

# Python (URL-safe base64、約 43 文字)
python3 -c 'import secrets; print(secrets.token_urlsafe(32))'

# /dev/urandom + xxd
head -c 32 /dev/urandom | xxd -p -c 64
```

短い token (16 文字未満等) や辞書語ベースの token は brute force の標的に
なるため避ける。staging と production は **必ず別の値** にして、staging で
漏れた token が production に通用しない分離を保つ。

## 3. 登録 (初回 / rotation 共通)

`vp` は本リポの開発者環境で `pnpm exec` 相当を担う wrapper (詳細は
[`docs/csa-server/deployment.md`](deployment.md) §1 参照、`vp exec wrangler X`
と `pnpm exec wrangler X` は等価)。`vp` が無い環境では `pnpm exec` で読み替えてよい。

`vp exec wrangler login` を済ませた後で、対象環境の toml を指定して
`wrangler secret put` を実行する。

```bash
cd crates/rshogi-csa-server-workers

# staging
vp exec wrangler secret put ADMIN_API_TOKEN --config wrangler.staging.toml

# production (staging とは別値)
vp exec wrangler secret put ADMIN_API_TOKEN --config wrangler.production.toml
```

プロンプトに値を入力 (echo されない)。Cloudflare 側で encrypted at rest され、
Worker code は `env.var(ConfigKeys::ADMIN_API_TOKEN)` で参照する (var / secret
は同 namespace に展開される Cloudflare 仕様)。

## 4. rotation 手順

旧 token grace 期間は持たない設計のため、以下の順序で実施する:

1. 新 token を §2 の手順で生成。
2. `wrangler secret put ADMIN_API_TOKEN --config wrangler.<env>.toml` を実行。
   **実行が成功した瞬間に Cloudflare 側で旧 token は即時無効化される (猶予期間
   なし)**。Worker は本 secret を `env.var()` で都度参照するため、追加の deploy
   は不要 (次の HTTP/WS リクエストから新 token のみが有効)。
3. 利用側 (運用 client / CI / cron 等) の保管値を新 token に差し替える。
   **手順 2 と 3 の間は admin 経路がすべて `PERMISSION_DENIED` を返す**ため、
   ラグを最小化する。
4. 1 局 / 1 endpoint 通電して動作確認 (例: HTTP admin endpoint の 200、または
   WS 内 admin command が `##[ADMIN] OK` を返す)。

複数オペレータが旧 token を保持している運用なら、rotation 直前に共有チャネル
(Slack 等) でアナウンスし、即時切替できる体制を整える。

## 5. 削除 / 無効化

```bash
vp exec wrangler secret delete ADMIN_API_TOKEN --config wrangler.<env>.toml
```

削除後は admin 認可が必要な経路がすべて
[`AdminAuthError::TokenNotConfigured`](../../crates/rshogi-csa-server-workers/src/admin_auth.rs)
で fail-closed する (404 / 拒否で隠蔽)。即時 kill-switch として有効。

## 6. 確認

```bash
# 登録済 secret 一覧 (値は表示されない)
vp exec wrangler secret list --config wrangler.<env>.toml
```

`ADMIN_API_TOKEN` が一覧に含まれていない環境では admin 経路は通電しない。

## 7. 整合性 gate

整合性 test (`tests/wrangler_environment_toml_consistency.rs`) が、
`wrangler.production.toml` / `wrangler.staging.toml` の `[vars]` に
`ADMIN_API_TOKEN` が混入していたら CI で fail させる契約。本値は必ず secret 経由
で配置し、env toml の `[vars]` テーブルには書かない。

`wrangler.toml.example` (local dev template) には placeholder として
`[vars]` に書くのが正しい運用 (`tests/wrangler_template_consistency.rs` で
gate 済み)。

## 8. 関連

- 認可ロジック仕様: [`crates/rshogi-csa-server-workers/src/admin_auth.rs`](../../crates/rshogi-csa-server-workers/src/admin_auth.rs)
- WS 内 admin command 実装: [`crates/rshogi-csa-server-workers/src/game_room.rs`](../../crates/rshogi-csa-server-workers/src/game_room.rs) (`handle_admin_elevation` / `upgrade_attachment_to_admin`)
- Cloudflare secret 全般: [`docs/csa-server/deployment.md`](deployment.md) §2.5
- 関連 issue: [#560](https://github.com/SH11235/rshogi/issues/560) (foundation), [#621](https://github.com/SH11235/rshogi/issues/621) (本コマンド)
