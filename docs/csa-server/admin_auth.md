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

`%%ADMIN [<token>]` 受信時の応答:

| ケース | 応答 |
|---|---|
| `verify_admin_token_str` 成功 (token 一致) | `##[ADMIN] OK` + `##[ADMIN] END` (session を admin 昇格) |
| token 不一致 / secret 未配置 / token 部欠落 (`%%ADMIN` 単体 / whitespace のみ) | `##[ADMIN] PERMISSION_DENIED` + `##[ADMIN] END` |

失敗ケースを uniform `PERMISSION_DENIED` に統一する理由は、
`%%ADMIN` (silent) と `%%ADMIN <wrong>` (response) を分岐すると、attacker
からは応答有無で「`%%ADMIN` が認識される command」かが推定できてしまうため。
`TokenNotConfigured` / `MissingCredential` / `TokenMismatch` を区別せず同じ
応答を返すことで「admin 機能が configured かどうか」も含めて leak を防ぐ。

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

## 9. LOGIN handle whitelist (issue #664)

[#664](https://github.com/SH11235/rshogi/issues/664) で導入された
`WORKERS_HANDLE_AUTH` 環境変数による LOGIN handle 自称防止機構の運用手順。
親 [#621](https://github.com/SH11235/rshogi/issues/621) で resolve した
`ADMIN_HANDLE` 平文露出 + `%%ADMIN <token>` 経路の続編として、Floodgate
operator handle (`floodgate`, `wdoor` 等) の **第三者による自称** を防ぐ。

### 9.1 機能サマリ

- 登録 handle に限り `LOGIN <handle>+<game_name>+<color> <password>` /
  `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` の password を
  **SHA256 比較** する。
- whitelist に **無い** handle は従来通り self-claim で素通し
  (Floodgate 互換 client / 一般対局者の挙動を変えない後方互換最優先)。
- env JSON 不正は **fail-closed** で全 LOGIN reject (`LOGIN:incorrect handle_auth_failed`)。
- 私的対局経路 (`LOGIN_LOBBY <handle>+private-<token>+free`) は token 経由で
  `inviter` / `opponent` が決定論的に固定されるため、whitelist 対象外。
- 対象は **Workers のみ**。TCP frontend (`crates/rshogi-csa-server-tcp`) は
  既存の `admin_handles` 機構を継続利用し、本機構は適用しない。

### 9.2 Schema

```json
[
  {"handle":"alice","password_sha256":"<lowercase hex 64 chars>"},
  {"handle":"floodgate","password_sha256":"<lowercase hex 64 chars>"}
]
```

- `handle`: LOGIN の `<handle>` 部 (例: `alice+game-eval+black` の `alice`)。空文字 NG。
- `password_sha256`: **lowercase hex 64 chars** 固定 (入力サーフェスを絞り
  typo を弾きやすくする目的で uppercase / base64 は許容しない)。
- 同一 `handle` の重複は parse エラーで全 LOGIN reject (fail-closed)。

### 9.3 SHA256 ハッシュ作成手順

```bash
# 改行を含めない (echo -n が肝)。GNU coreutils を使う場合:
echo -n "your-password-here" | sha256sum | awk '{print $1}'

# macOS (shasum 経由):
echo -n "your-password-here" | shasum -a 256 | awk '{print $1}'
```

> ⚠️ `echo "password" | sha256sum` (改行付き) は **異なる hash** を出すので
> 必ず `-n` を付けること。`$'...'` 形式 (`echo $'no\n trailing'`) など改行が
> 紛れる経路にも注意。

### 9.4 設定方法

#### production / staging

`WORKERS_HANDLE_AUTH` は password hash を含むため `wrangler.<env>.toml` の
`[vars]` には書かない。Cloudflare secret として配置する:

```bash
# 1 行 JSON を heredoc 等で渡す。複数 entry は `,` 区切り。
vp exec wrangler secret put WORKERS_HANDLE_AUTH --config wrangler.production.toml <<'EOF'
[{"handle":"alice","password_sha256":"6e9b54475e7e568f848f7c302c6d899d85c1118dd39b7b46272ba0b1d9b10c43"}]
EOF
```

整合性 test (`tests/wrangler_environment_toml_consistency.rs`) が
`wrangler.production.toml` / `wrangler.staging.toml` の `[vars]` に
`WORKERS_HANDLE_AUTH` が混入していたら CI で fail させる契約。

#### local dev

`wrangler.toml.example` の `[vars]` に空配列 placeholder (`"[]"`) を残してある。
local で whitelist 経路を試したいときだけ `cp wrangler.toml.example wrangler.toml`
した後の `wrangler.toml` を直接書き換える (実値はコミットされない)。

### 9.5 Migration 手順 (空 → 設定)

1. 既存 deploy には `WORKERS_HANDLE_AUTH` secret が未配置な状態 (= self-claim 既定)。
2. admin operator handle (例: `alice`) の password を決め、SHA256 hash を作成。
3. `wrangler secret put WORKERS_HANDLE_AUTH --config wrangler.staging.toml` で
   staging に配置。`alice` 以外の handle は影響を受けないことを smoke 検証。
4. `alice` の正しい password / 不正 password で staging の LOGIN を通電確認:
   - 正しい: `LOGIN:alice+game-eval+black OK`
   - 不正: `LOGIN:incorrect handle_auth_failed` + 1003 close
5. production に同じ secret を配置。

### 9.6 Rotation 手順

password を交代する場合は、旧 entry と新 entry を **一時的に並走** させて
client 側の切替を待つ:

1. 新 password の SHA256 hash を計算。
2. 並走期間用に新しい `handle` 名 (例: `alice-2026q3`) を割り当てた entry を
   追加して `wrangler secret put WORKERS_HANDLE_AUTH ...` を更新。同じ `handle`
   名で hash を上書きすると旧 password が通らなくなるため、並走期間中は
   別 handle として扱う。
3. client 側を新 handle + 新 password で切替。
4. 旧 entry を削除して再 `wrangler secret put`。

### 9.7 Wire format

password mismatch / env JSON 不正は WS に下記を返して 1003 close する:

```text
LOGIN:incorrect handle_auth_failed       ← GameRoom (`LOGIN <handle> <password>`)
LOGIN_LOBBY:incorrect handle_auth_failed ← LobbyDO (`LOGIN_LOBBY <handle> <password>`)
```

silently allow / silently reject は採らず必ず明示 reason を返す
(`docs/csa-server/protocol-reference.md` の reason 一覧に固定済み)。

### 9.8 関連

- whitelist 実装: [`crates/rshogi-csa-server-workers/src/handle_auth.rs`](../../crates/rshogi-csa-server-workers/src/handle_auth.rs) (`HandleAuthRegistry`)
- LOGIN 受理 hook: [`crates/rshogi-csa-server-workers/src/game_room.rs`](../../crates/rshogi-csa-server-workers/src/game_room.rs) (`enforce_handle_auth`)
- LOGIN_LOBBY 受理 hook: [`crates/rshogi-csa-server-workers/src/lobby.rs`](../../crates/rshogi-csa-server-workers/src/lobby.rs) (`enforce_lobby_handle_auth`)
- 関連 issue: [#664](https://github.com/SH11235/rshogi/issues/664) (本機構), [#621](https://github.com/SH11235/rshogi/issues/621) (親)
