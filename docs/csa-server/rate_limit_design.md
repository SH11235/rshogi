# Rate limit / abuse protection の設計

Workers 版 CSA サーバーの接続・コマンド頻度制限について、実装上の責務と
採用理由をまとめる。環境変数の変更・観測・障害時の運用は
[rate limit 運用 Runbook](rate_limit.md) を参照。

## 1. スコープ

対象は LOGIN_LOBBY、CHALLENGE_LOBBY、player / spectator の room upgrade、
および player の room upgrade を使った room 作成頻度の抑制。
WAF の契約・設定、viewer API の制限、管理コマンドの制限は別の責務とする。

## 2. 攻撃モデルと防御

### 2.1 抑制する負荷

- 同一 IP からの大量 upgrade による接続・Durable Object の負荷。
- 同一 IP / handle からの LOGIN_LOBBY の連続試行。
- 同一 IP / inviter からの CHALLENGE_LOBBY の連続発行。

対局開始前の滞留時間制限、WebSocket のメッセージ長制限、orphan の回収は
頻度制限だけでは抑えられない負荷を扱うため、併用する。

### 2.2 防御層

- 外部の WAF 等は追加の防御層であり、Worker 内の制限はそれに依存しない。
- Worker では専用 RateLimiter DO の token bucket で制限する。
- room 作成負荷は player route に別の厳しい上限を設けて抑える。
  これは実際の DO 新規作成数ではなく、player upgrade 回数を代理指標とする制限。

### 2.3 共通実装ガード

- IP の取得元は `CF-Connecting-IP` のみとし、クライアント入力の
  `X-Forwarded-For` を鍵に使わない。
- IP が欠落した場合は fail-closed。room upgrade では HTTP 503 と
  `Retry-After: 10`、Lobby の対象コマンドでは rate_limited 応答を返す。
- DO の取得・RPC・応答解析のエラーは成功判定に変換せず伝播する。
- 不正な route や Origin、Upgrade の検査を通過してから bucket を消費する。

## 3. 設計判断

### Q1. 外部 WAF との関係

WAF の利用可否・ルール・権限は配備先の運用に属する。アカウントの契約変更を
前提にせず、Worker code-only の制限を基礎とする。

### Q2. 専用 RateLimiter DO

同じ `(kind, identifier)` は `{kind}:{identifier}` を名前とした同一の DO へ送る。
制限の種類と識別子で分散することで、異なる IP / handle の bucket を分離する。
各 room 内だけの counter では room を跨ぐ同一 IP を集約できないため、
GameRoom / Lobby とは別の DO にする。

token bucket は capacity を1分あたりの上限とし、capacity / 60 token/sec で
連続的に補充する。満タン時の burst は許容するが、固定窓の境界で一括リセットはしない。
状態は storage に保存し、メモリ上の cache が失われても復元する。
共有 counter の更新を取りこぼさないため、単純な分散 KV の read-modify-write では代替しない。

実装: [rate_limit.rs](../../crates/rshogi-csa-server-workers/src/rate_limit.rs)。

### Q3. 閾値と対象

| 環境変数 | 対象 | 既定値 / 分 |
|---|---|---:|
| `LOBBY_LOGIN_RATE_PER_IP_PER_MIN` | LOGIN_LOBBY / IP | 10 |
| `LOBBY_LOGIN_RATE_PER_HANDLE_PER_MIN` | LOGIN_LOBBY / handle | 5 |
| `LOBBY_CHALLENGE_RATE_PER_IP_PER_MIN` | CHALLENGE_LOBBY / IP | 5 |
| `LOBBY_CHALLENGE_RATE_PER_HANDLE_PER_MIN` | CHALLENGE_LOBBY / inviter | 3 |
| `ROOM_CREATE_RATE_PER_IP_PER_MIN` | player room upgrade / IP | 20 |
| `WS_ROOM_UPGRADE_RATE_PER_IP_PER_MIN` | player + spectator room upgrade / IP | 60 |

閾値は `RateLimitThresholds::DEFAULTS` と環境別 wrangler 設定で定義する。
空・非数値・0・範囲外の値は安全側の既定値に戻す。0 は無効化を意味しない。

### Q4. 拒否応答

- room upgrade: HTTP 503 と `Retry-After: <sec>`。
- LOGIN_LOBBY: `LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>`。
- CHALLENGE_LOBBY: `CHALLENGE_LOBBY:incorrect rate_limited retry_after=<sec>`。

Lobby の頻度超過では接続を維持し、クライアントが待機して再試行できるようにする。
拒否応答の生成は [rate_limit.rs](../../crates/rshogi-csa-server-workers/src/rate_limit.rs) に集約する。

### Q5. クライアントの待機

csa-client はサーバーが示す retry-after を再接続待機へ反映する。
外部クライアントの実装は保証できないため、待機の実施に関係なくサーバー側で制限する。
クライアントの仕様は [csa-client](../csa-client.md) を参照。

## 4. 組込み箇所

### 4.1 他の防御との分離

メッセージサイズ制限、AGREE の期限、観戦者数制限、認証は頻度制限とは独立に維持する。

### 4.2 観測

`rate_limit_denied` と `rate_limit_missing_cf_ip` を構造化ログに出す。
監視の集計・通知方法は [observability](observability.md) に従う。

### 4.3 bucket の独立性

IP と handle / inviter の制限を順に適用する。先の bucket を消費した後で
後の bucket が拒否しても、先の消費を取り消す複数 DO 間の transaction は行わない。

### 4.4 Implementation hook points

| 経路 | 検査順序 |
|---|---|
| room の player / spectator upgrade | route・Origin・Upgrade 検査 → IP 取得 → WsRoomUpgradePerIp → player のみ RoomCreatePerIp → GameRoom DO 解決・fetch |
| LOGIN_LOBBY | attachment の IP を取得 → LobbyLoginPerIp → LobbyLoginPerHandle → 認証・queue 登録 |
| CHALLENGE_LOBBY | attachment の IP を取得 → LobbyChallengePerIp → LobbyChallengePerInviter → 招待発行 |

room 作成 cap は Lobby の MATCHED 送出時ではなく、その後の player upgrade で消費する。
spectator は RoomCreatePerIp を消費しないが、WsRoomUpgradePerIp は player と共有する。

実装: [router.rs](../../crates/rshogi-csa-server-workers/src/router.rs)、
[lobby.rs](../../crates/rshogi-csa-server-workers/src/lobby.rs)。

## 5. 検証条件

### 5.1 設定

既定値、不正値の fallback、環境別 wrangler 設定のキー整合性を検査する。

### 5.2 Token bucket

消費、経過時間に応じた補充、飽和、retry-after の算出、storage エラー時の
挙動を host unit test と DO の統合テストで検査する。

### 5.3 Miniflare E2E

[rate_limit.test.ts](../../crates/rshogi-csa-server-workers/tests/miniflare_smoke/rate_limit.test.ts)
を中心に以下を検査する。5 の時間経過による補充は Miniflare では実行せず、
host の pure logic test で補う。

1. 同一 IP からの LOGIN_LOBBY burst の拒否。
2. 異なる IP でも同一 handle の LOGIN_LOBBY 制限。
3. CHALLENGE_LOBBY の IP / inviter ごとの制限。
4. room upgrade の HTTP 503 と Retry-After。
5. 時間経過による token の補充 (host test)。
6. CF-Connecting-IP 欠落時の fail-closed。
7. 不正な room_id が bucket を消費しないこと。

実機で burst を確認する場合、補充が消費に追いつく逐次送信だけで判断しない。
実行時の閾値・送信間隔・応答を合わせて記録する。
