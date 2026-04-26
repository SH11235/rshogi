# Workers staging × csa_client 実機対局 E2E 手順書

CSA-over-WebSocket で Cloudflare Workers の staging サーバーに `csa_client` ×
2 セッションを繋ぎ、平手 1 対局を最後まで通電して棋譜が R2 (`rshogi-csa-kifu-staging`)
に書き出されることを実機で確認する手順をまとめる。

deployment 全体像は [`deployment.md`](deployment.md) を参照。

## 0. 前提

- staging Worker (`rshogi-csa-server-workers-staging.<account>.workers.dev`)
  が deploy 済みであること。
- ローカルに USI エンジン（`rshogi-usi` など）の release バイナリがあり、
  `/path/to/rshogi-usi` で起動できること。
- `wrangler` CLI（または `vp exec wrangler`）で staging Worker / R2 bucket に
  操作できる権限があること。
- `csa_client` の WS 経路（`tungstenite` 依存）が main に取り込まれていること。

## 1. staging Worker の Origin allowlist を一時調整

`csa_client` は WebSocket Upgrade 時に `Origin` ヘッダを送る。staging Worker は
`CORS_ORIGINS` の allowlist に一致しない Origin を `403 Forbidden Origin` で
弾くため、E2E 実施時のみ csa_client の Origin を allowlist に追加する。

```bash
# 既存値を確認
vp exec wrangler --config crates/rshogi-csa-server-workers/wrangler.staging.toml \
  whoami
# secret や var の更新は wrangler.staging.toml 経由でしか入らないため、
# 一時的に `CORS_ORIGINS` を編集 → deploy → 確認 → 巻き戻す流れになる。
```

`crates/rshogi-csa-server-workers/wrangler.staging.toml` の `[vars]` セクションで
`CORS_ORIGINS` を以下のように一時更新（commit せずに手元で書き換えるだけで OK）。

```toml
[vars]
CORS_ORIGINS = "https://csa-client-local"
```

そのまま CI 経由 (`workflow_dispatch` で staging deploy を起動) または手元から
`vp exec wrangler --config wrangler.staging.toml deploy` で適用する。

E2E が完了したら `CORS_ORIGINS = ""`（または元の値）に戻して再 deploy する。

## 2. ローカル csa_client × 2 を用意する

同一 `room_id` に黒 (`+`) と白 (`-`) で 1 セッションずつログインさせる。
それぞれ独立の TOML を用意し、別ターミナルから起動する。

### 2-1. 黒番 (`crates/tools/examples/csa_client_staging/staging-black.toml.example`)

`crates/tools/examples/csa_client_staging/` 配下に同梱した
`staging-black.toml.example` をコピーして使う。

```bash
cp crates/tools/examples/csa_client_staging/staging-black.toml.example \
   /tmp/staging-black.toml
# `/tmp/staging-black.toml` の `engine.path` だけローカルの USI binary パスに合わせて編集。
```

### 2-2. 白番 (`crates/tools/examples/csa_client_staging/staging-white.toml.example`)

```bash
cp crates/tools/examples/csa_client_staging/staging-white.toml.example \
   /tmp/staging-white.toml
# 同様に engine.path をローカル binary に合わせる。
```

### 2-3. 黒・白の `id` / `password`

CSA Workers では「ハンドル名 + パスワード」だけで合意成立する（ハンドル名
照合のみ、パスワードは検証されない）。E2E 用には `id = "csa_e2e_black_<日付>"`
等の高エントロピー値を双方の TOML に書く。`%%SETBUOY` の操作は不要なので、
`ADMIN_HANDLE` と被らないユニークな値を使う。

例：
- `id = "csa_e2e_black_20260427"`、`password = "anything"`
- `id = "csa_e2e_white_20260427"`、`password = "anything"`

## 3. 同一 room_id で接続して対局を 1 局走らせる

`server.host` に同じ `wss://...workers.dev/ws/<room_id>` を指定して、双方の
csa_client を別ターミナルで起動する。`room_id` は新規生成する任意の文字列
（`e2e-20260427-001` など）。

ターミナル A（黒番）:

```bash
cargo run -p tools --release --bin csa_client -- /tmp/staging-black.toml
```

ターミナル B（白番）:

```bash
cargo run -p tools --release --bin csa_client -- /tmp/staging-white.toml
```

成立すれば双方のログに以下の流れが流れる:

```
[CSA/WS] 接続中: wss://...workers.dev/ws/e2e-20260427-001
[CSA/WS] 接続成功: status=101 Switching Protocols
[CSA] ログイン成功: csa_e2e_black_20260427
[CSA] 対局待機中...
[CSA] 対局情報受信: <game_id> ... csa_e2e_black_20260427 vs csa_e2e_white_20260427 ...
[CSA] 対局開始: START:<game_id>
...
```

平手 1 対局を最後まで進めるとどちらか一方が `%TORYO` を送り、Worker が
`#WIN` / `#LOSE` を返して `END Game_Summary` に到達する。

## 4. R2 棋譜が書き出されたことを確認する

Worker は終局時に `KIFU_BUCKET` (`rshogi-csa-kifu-staging`) に CSA V2 棋譜を
書き出す。bucket の object 一覧を取得して直近のキーを確認する:

```bash
vp exec wrangler r2 object list rshogi-csa-kifu-staging \
  --config crates/rshogi-csa-server-workers/wrangler.staging.toml | head
```

直近の object キー（例: `2026-04-27/<game_id>.csa`）を取得して中身を確認:

```bash
vp exec wrangler r2 object get \
  rshogi-csa-kifu-staging/2026-04-27/<game_id>.csa \
  --config crates/rshogi-csa-server-workers/wrangler.staging.toml \
  --file /tmp/<game_id>.csa
cat /tmp/<game_id>.csa
```

CSA V2 形式（`'`コメント、`+7776FU,T<sec>` ...、`%TORYO` / `+SUMI` 等の
終局コマンド）が書かれていれば成功。

加えて、Floodgate 履歴 (`FLOODGATE_HISTORY_BUCKET`) にも 1 オブジェクト
追加されていることを確認する:

```bash
vp exec wrangler r2 object list rshogi-csa-floodgate-history-staging \
  --config crates/rshogi-csa-server-workers/wrangler.staging.toml | head
```

## 5. 後始末

1. `wrangler.staging.toml` の `CORS_ORIGINS` を元の値（通常は空文字列または運用値）に戻し、再 deploy する。
2. R2 bucket の `csa_e2e_*` 関連オブジェクトを削除したい場合は
   `vp exec wrangler r2 object delete` で個別に削除できる（残しておいても害はない）。
3. ローカルに残った `/tmp/staging-black.toml` / `/tmp/staging-white.toml` /
   棋譜ファイルを破棄する。

## トラブルシューティング

| 症状 | 原因候補 | 対処 |
| --- | --- | --- |
| `CSAサーバー接続失敗: WebSocket Upgrade 失敗` (`403 Forbidden Origin`) | `CORS_ORIGINS` allowlist に csa_client の Origin が無い | §1 で staging に Origin を追加してから再 deploy |
| `ログイン失敗: LOGIN:INCORRECT` 等 | サーバー側 league で同一ハンドルが既に接続中 | `id` を別値に変えるか、Worker の DO state を `vp exec wrangler ...` で再起動する |
| 双方接続するも対局が始まらない | `room_id` が一致していない | URL の `/ws/<room_id>` 部分が両 toml で完全一致しているか確認 |
| 対局終局後も R2 に書き込まれない | DO storage の終局イベントが落ちた可能性 | Worker のログを `vp run tail:staging` で確認 |
