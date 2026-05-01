# docs/csa-server/ 索引

`rshogi-csa-server` (TCP / Cloudflare Workers 共通 core) と、その上の
`rshogi-csa-server-tcp` / `rshogi-csa-server-workers` に関するドキュメント
索引。各 doc の責務を 1 行で示す。

## CSA プロトコル仕様

- [`protocol-reference.md`](protocol-reference.md) — 本リポが受理する標準 CSA / x1 拡張 / 独自拡張 (Reconnect_Token / `BEGIN Reconnect_State` / Lobby `MATCHED`) を実装位置 (`file:line`) 付きで一覧する利用者向け参照 doc。**新規メンバの最初の 1 枚**。

## 設計 / 運用 (Workers)

- [`deployment.md`](deployment.md) — Cloudflare Workers の staging / production 構築・運用 runbook。
- [`viewer_access_control.md`](viewer_access_control.md) — viewer / spectate API の access control (Origin allowlist / kill-switch) 運用。
- [`lobby_design.md`](lobby_design.md) — LobbyDO + マッチングの詳細設計 (`/ws/lobby`、`MATCHED` 通知、queue 戦略)。
- [`lobby_e2e_runbook.md`](lobby_e2e_runbook.md) — Lobby マッチング対局を実機 staging で回す E2E 運用手順。
- [`staging-e2e.md`](staging-e2e.md) — Workers staging × `csa_client` 実機対局シナリオ集 (平手 / 連続対局 / 切断再接続 / Buoy / 観戦)。
- [`clock_defaults.md`](clock_defaults.md) — staging / production の clock 既定差分 (ops 仕様 single source of truth)、Floodgate 互換性、`CLOCK_PRESETS` 上書き経路。

## 関連 doc (このディレクトリ外)

- [`../csa-client.md`](../csa-client.md) — `csa_client` (CSA client 実装) の使い方。
- 各 crate 直下の Rust doc コメント (`rustdoc`) — 型 / 関数の最終契約はソース側を一次ソースとする。
