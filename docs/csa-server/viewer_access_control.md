# viewer 配信 / spectate API access control 運用 Runbook

`rshogi-csa-server-workers` が提供する viewer / spectate 系 endpoint に対する
production / staging 環境のアクセス制御方針と、rollout / kill-switch 手順を
まとめる。基本的な deploy 手順は [`deployment.md`](deployment.md)、Workers
deploy 環境への実機通電は repo 同梱 Skill
`.claude/skills/csa-e2e-staging/SKILL.md` を参照。

## 1. access control が及ぶ endpoint

以下はすべて同一の access control（`ALLOW_VIEWER_API` opt-in と
`WS_ALLOWED_ORIGINS` の必須化）を共有する:

| Endpoint | 用途 |
| --- | --- |
| `GET /api/v1/games` | 終局済対局の一覧（pagination） |
| `GET /api/v1/games/live` | 進行中対局の一覧（pagination） |
| `GET /api/v1/games/<game_id>` | 終局済対局の単局取得（CSA 本文 + meta） |
| `GET /ws/<id>/spectate` | 観戦 WebSocket 接続（progress broadcast 受信） |

Player 経路（`/ws/<room_id>`、ネイティブ CSA クライアントが LOGIN する経路）は
本 access control の対象外で、既存の Origin 検査 semantics を維持する
（Origin ヘッダを送らないネイティブクライアントを温存）。

## 2. 環境変数の責任分界

| 変数 | 役割 | 既定値（fail-closed） |
| --- | --- | --- |
| `ALLOW_VIEWER_API` | viewer / spectate endpoint の有効化 / 無効化フラグ。`true` / `1` / `yes` / `on` で有効、それ以外（空 / `false` / 不正値）は無効。 | 不正値 → 無効 + `console_log` に警告 |
| `WS_ALLOWED_ORIGINS` | viewer / spectate へのアクセス可能 Origin 集合（CSV）。viewer / spectate では **必須**。Player 経路は空でもネイティブクライアントを許可（既存 semantics）。 | 空 → viewer / spectate を 403 |

挙動マトリクス（viewer / spectate 経路）:

| `ALLOW_VIEWER_API` | `WS_ALLOWED_ORIGINS` | 結果 |
| --- | --- | --- |
| 無効・未設定・不正値 | * | **404**（kill-switch、既存ルーティングへフォールスルー） |
| 有効 | 空 | **403**（fail-closed） |
| 有効 | 非空、Origin 不一致 | **403** |
| 有効 | 非空、Origin 一致 / Origin 未送信（curl 等） | **通過** |

## 3. rollout チェックリスト

新環境への初回 rollout、もしくは既存環境で viewer / spectate を有効化する際に
順に実施する。実 deploy の確認結果は本 runbook ではなく PR / Issue / 運用監視
の責務で残すこと。

1. `wrangler.<env>.toml` に以下が **両方** 揃っていることを PR で確認する:
   - `[vars] ALLOW_VIEWER_API = "1"`（kill-switch 時は `"0"` に切替）
   - `[vars] WS_ALLOWED_ORIGINS = "https://<client-origin>[,...]"`（非空、最終 client URL）
2. staging 環境に deploy し、smoke チェック（§5）を通過させる。
3. production 環境に deploy する前に `WS_ALLOWED_ORIGINS` の値が production
   client の実 URL と一致していることを再確認する（staging 値が混入していないか）。
4. production deploy 後、smoke チェック（§5）の各 curl パターンを通電させ、
   想定通りのステータス遷移であることを確認する。
5. 一定期間 access log を観察し、想定外 Origin からの 403 が連続していないか
   モニタリングする。

## 4. kill-switch 手順

production 中に viewer / spectate を即時無効化する必要が生じた場合:

1. `wrangler.production.toml` で `ALLOW_VIEWER_API = "0"` に書き換える PR を作成。
2. merge → 自動 deploy or `workflow_dispatch` で再 deploy。
3. deploy 完了後、§5 の smoke check で 4 endpoint すべてが **404** を返すことを
   確認する。404 で揃うことが kill-switch 成立のシグナル。

`WS_ALLOWED_ORIGINS = ""` でも fail-closed で 403 を返すが、404 と比べて運用上の
意図（無効化したのか設定漏れか）が判別しづらいため、kill-switch には
`ALLOW_VIEWER_API` を使うこと。

## 5. smoke 確認 curl コマンド例

`<host>` は `rshogi-csa-server-workers.<account>.workers.dev`、
`<allowed-origin>` は `WS_ALLOWED_ORIGINS` に登録した Origin、
`<other-origin>` は登録していない Origin に置き換える。

### 5-1. viewer API 無効時（`ALLOW_VIEWER_API="0"`）

```bash
# 404 を期待
curl -sS -o /dev/null -w "%{http_code}\n" "https://<host>/api/v1/games"
curl -sS -o /dev/null -w "%{http_code}\n" "https://<host>/api/v1/games/live"
curl -sS -o /dev/null -w "%{http_code}\n" "https://<host>/api/v1/games/sample-id"
```

### 5-2. viewer API 有効時 + Origin allowlist 未設定

```bash
# 403 を期待（fail-closed）
curl -sS -o /dev/null -w "%{http_code}\n" \
  -H "Origin: https://<other-origin>" \
  "https://<host>/api/v1/games"
```

### 5-3. viewer API 有効時 + Origin 一致

```bash
# 200 を期待
curl -sS -o /dev/null -w "%{http_code}\n" \
  -H "Origin: https://<allowed-origin>" \
  "https://<host>/api/v1/games?limit=1"
curl -sS -o /dev/null -w "%{http_code}\n" \
  -H "Origin: https://<allowed-origin>" \
  "https://<host>/api/v1/games/live?limit=1"
```

### 5-4. viewer API 有効時 + Origin 不一致

```bash
# 403 を期待
curl -sS -o /dev/null -w "%{http_code}\n" \
  -H "Origin: https://<other-origin>" \
  "https://<host>/api/v1/games"
```

### 5-5. spectate WS（参考）

WebSocket Upgrade を curl で完結させるのは難しいため、Origin 付き接続テストは
repo 同梱 Skill `.claude/skills/csa-e2e-staging/SKILL.md` §5 の `wscat` 手順に
従う。viewer API 無効時は
`/ws/<id>/spectate` も 404 で揃うことだけ smoke チェックで担保する:

```bash
# Upgrade なしの GET でも 404 / 403 / 426 が状況に応じて返る
curl -sS -o /dev/null -w "%{http_code}\n" "https://<host>/ws/<room>/spectate"
```

## 6. 将来計画

- private 棋譜（個人対局）と public 棋譜（Floodgate 経由等）の分離は、本
  access control の上位レイヤとして別 Issue で扱う。本 runbook は public 棋譜
  のみを viewer 配信する前提に立つ。
- API token / shared secret 経由の認可は現時点では未導入。Origin allowlist で
  必要な防御を担保する設計に閉じている。
