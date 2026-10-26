# Workers のレート制限

CSA 接続・ロビー操作の頻度と、ログイン前の接続保持数を制限する。
制限に当たったクライアントは `Retry-After` / `retry_after` の秒数を待って再試行する。
仕組みの詳細は [rate limit の実装](rate_limit_design.md) を参照。

## ローカルで動作を確認する

本番へ負荷をかけず、Miniflare で LOGIN・CHALLENGE・upgrade の拒否応答を確認できる。
Node は crate の `.node-version`、pnpm は `package.json` の `packageManager` に合わせる。
repo root から次を実行する。

```bash
rustup target add wasm32-unknown-unknown
cargo install worker-build@=0.8.1 --locked
cd crates/rshogi-csa-server-workers
pnpm install --frozen-lockfile
pnpm exec vitest run --config tests/miniflare_smoke/vitest.config.ts \
  rate_limit.test.ts lobby_pending.test.ts
```

テストの global setup が Worker をビルドし、一時ストレージ上でサーバーを起動する。
`rate_limit.test.ts` は IP / handle / inviter 別の拒否と HTTP 503 + `Retry-After`、
`lobby_pending.test.ts` は未認証接続の上限・期限、休眠復帰、DO へ直接渡した IP 欠落を検査する。
Vitest の成功件数を確認し、Ctrl+C で中断した場合は同じコマンドでやり直す。

token の時間経過による補充・飽和・待ち時間の計算は、repo root で次を実行する。

```bash
cargo test -p rshogi-csa-server-workers rate_limit::tests
```

HTTP upgrade だけでは LOGIN の頻度制限を検証できない。実機で調べる場合も
WebSocket 上で対象のコマンドを送信し、閾値・送信間隔・応答を合わせて確認する。
Cloudflare edge を通る接続では `CF-Connecting-IP` は edge 側の値になる。

## 環境変数

`wrangler.staging.toml` / `wrangler.production.toml` の `[vars]` で設定する。
両環境の同梱値と、未設定・不正値のフォールバック値は次のとおり。

| 環境変数 | 対象 | 既定値 / 分 |
|---|---|---:|
| `LOBBY_LOGIN_RATE_PER_IP_PER_MIN` | LOGIN_LOBBY / IP | 10 |
| `LOBBY_LOGIN_RATE_PER_HANDLE_PER_MIN` | LOGIN_LOBBY / handle | 5 |
| `LOBBY_CHALLENGE_RATE_PER_IP_PER_MIN` | CHALLENGE_LOBBY / IP | 5 |
| `LOBBY_CHALLENGE_RATE_PER_HANDLE_PER_MIN` | CHALLENGE_LOBBY / inviter | 3 |
| `ROOM_CREATE_RATE_PER_IP_PER_MIN` | player の room upgrade / IP | 20 |
| `WS_ROOM_UPGRADE_RATE_PER_IP_PER_MIN` | Lobby / GameRoom upgrade / IP（別 bucket） | 60 |

値は `u32` の正数。`0`、空、非数値、範囲外は既定値へ戻る。
0 による無効化はできない。実質的に緩和する場合は、必要な上限値を明示する。
型と既定値は `rate_limit.rs::RateLimitThresholds`、設定のキー整合は
`tests/integration/wrangler_environment_toml_consistency.rs` で検査している。

各 bucket は満タンから既定値分の burst を許し、1 分でその容量を補充する。
固定窓のリセット方式ではないため、送信中も少しずつ回復する。
IP と handle / inviter のチェックは順に実施し、後段で拒否されても
前段で消費した token は戻さない。

GameRoom の upgrade bucket は player / spectator で共有する。
player だけはさらに room 作成の bucket を消費する。これは新規 DO 数そのものではなく、
既存 room への再接続も含む player upgrade 回数を制限する。
Lobby upgrade は GameRoom upgrade・LOGIN・CHALLENGE の bucket と独立する。

## 閾値調整手順

1. `rate_limit_denied` の `kind`、IP、handle / inviter を確認し、制限の種類を特定する。
2. 対象環境の toml を変更し、PR を通して [デプロイ](deployment.md) する。
3. デプロイ完了後、拒否頻度と対局開始・再接続の挙動を確認する。

`[vars]` の編集だけでは配備中の値は変わらない。Worker が読んだ閾値を各 RPC で
RateLimiter DO に渡すため、デプロイ後に bucket ごとの移行操作は不要。
secret の更新で閾値を設定する構成ではない。
緩和は小幅ずつ行い、厳格化は staging で正規クライアントを拒否しないことを確認する。
staging からの実機確認は [ロビー対局手順](lobby_e2e_runbook.md) を参照。

## 拒否応答と障害時の挙動

| 経路 | 応答 |
|---|---|
| WebSocket upgrade | HTTP 503 + `Retry-After: <sec>` |
| LOGIN_LOBBY | `LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>` |
| CHALLENGE_LOBBY | `CHALLENGE_LOBBY:incorrect rate_limited retry_after=<sec>` |

Lobby の頻度超過では WebSocket を維持するが、後述の認証待ち期限は延長しない。
期限内に再試行できなければ、新たに接続し直す。

IP の取得元は `CF-Connecting-IP` のみで、`X-Forwarded-For` は使わない。
IP が欠落・空の upgrade は HTTP 503 + `Retry-After: 10` で拒否する。
DO RPC、JSON 応答解析、ストレージの失敗も許可に変換せずエラーとする
（この場合、通常の頻度超過と同じ応答になるとは限らない）。

## ログとトラブルシューティング

repo root から対象環境のログを表示する。`wrangler tail` は接続後のイベントを受信する。
過去ログの検索は [observability](observability.md) を参照。

```bash
cd crates/rshogi-csa-server-workers
pnpm exec wrangler tail --config wrangler.production.toml --format json \
  | jq -c '.logs[]?.message[]? | fromjson? // empty | select(.event == "rate_limit_denied")'
```

staging では `--config wrangler.staging.toml` に置き換える。
特定 IP だけに絞るときは jq の条件へ `and .ip == "203.0.113.50"` を加える。
top-level の `.event` は invocation のメタ情報なので、先に `logs[].message[]` を展開する。

| event | 意味 | 確認するフィールド |
|---|---|---|
| `rate_limit_denied` | bucket 超過 | `kind`, `ip`, `handle` / `inviter`, `retry_after_sec`, `path`（経路ごとに異なる） |
| `rate_limit_missing_cf_ip` | IP が取得できない経路での拒否 | `component`, `path` / `kind` |
| `websocket_upgrade_accepted` | Lobby の upgrade 成功 | `cf_ip_present` |
| `lobby_pending_limit_denied` | 未認証接続数の上限超過 | `total`, `per_ip` |

すべての利用者が拒否される場合は次を確認する。

- 設定値の typo / 0 / 範囲外で、意図せず既定値へ戻っていないか。
- 同じ NAT 配下のクライアントが IP 別 bucket を共有していないか。IP 別の制限に
  当たっている場合、handle 別だけを緩めても解消しない。IP ごとの例外設定はない。
- `CF-Connecting-IP` が取得できる Cloudflare の接続経路か。
- DO / ストレージの障害か。通常の超過ログと、RPC 等のエラーを区別する。

## ストレージの保持

RateLimiter は `(kind, identifier)` ごとに DO を分け、bucket の残量を保存する。
isolate が休眠・再構築されても残量は復元される。永続データを時間で自動削除する
仕組みはなく、異なる IP / handle が増えるほど保存対象も増える。
JSON のフィールドサイズだけから DO の総ストレージ使用量を見積もらず、
配備先の instance 数・storage 使用量・費用を確認する。
頻度制限とは別に、必要に応じて edge 側の制御も併用する。

## Lobby の未認証接続

LOGIN 前の接続は Lobby 全体で 100、同一 IP で 4 まで。超過は HTTP 503 +
`Retry-After: 30` で拒否する。NAT 配下でも未認証枠を共有するため、
接続後は速やかに LOGIN する。

認証待ちは accept から 30 秒で期限切れとなり、無送信でも close code 1008
（`login timeout`）で切断する。無効コマンド、binary、rate limit 後の再送、
CHALLENGE 発行では期限を延長しない。LOGIN 成功後は公開 queue / private challenge
固有の期限へ移る。

認証待ちの期限と IP は attachment に保存する。これらがない古い attachment は
次のイベントまたは alarm で拒否・切断する。古い無期限接続に alarm がない場合は、
更新後の最初のイベントまで走査されない。
認証待ち・公開 queue・challenge は、一つの alarm に最短期限を予約する。
公開 queue の heartbeat 時刻も attachment に保存し、休眠復帰後も保持する。

close 完了が遅れても期限切れ接続を認証・枠の集計へ戻さない。
保持上限は有効な認証待ち接続を対象とし、物理的な WebSocket の解放はランタイムに依存する。
複数 IP（IPv6 の別アドレスを含む）による全枠占有、IP prefix の集約、公平な受付枠の
割当てはこの制限で扱わない。拒否が継続する場合は edge 側の制御も検討する。

## 関連

- [実装の詳細](rate_limit_design.md)
- [クライアントの接続・再試行](../csa-client.md)
- [デプロイ・反映手順](deployment.md)
- [ログの検索](observability.md)
