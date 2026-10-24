# docs/csa-server/ 索引

`rshogi-csa-server` (TCP / Cloudflare Workers 共通 core) と、その上の
`rshogi-csa-server-tcp` / `rshogi-csa-server-workers` に関するドキュメント
索引。各 doc の責務を 1 行で示す。

## CSA プロトコル仕様

- [`protocol-reference.md`](protocol-reference.md) — 本リポが受理する標準 CSA / x1 拡張 / 独自拡張 (Reconnect_Token / `BEGIN Reconnect_State` / Lobby `MATCHED`) を実装位置 (`file:line`) 付きで一覧する利用者向け参照 doc。**新規メンバの最初の 1 枚**。

## 設計 / 運用 (TCP)

- [`tcp-server.md`](tcp-server.md) — `rshogi-csa-server-tcp`（TCP フロントエンド）の起動・運用ガイド。`players.toml` / 時計設定 / マッチング / 定刻ラウンド (`[[schedules]]`) / 駒落ち / Floodgate 運用機能。自前マシンで対局・評価を回す場合の入口。

## 設計 / 運用 (Workers)

- [`deployment.md`](deployment.md) — Cloudflare Workers の staging / production 構築・運用 runbook。
- [`admin_auth.md`](admin_auth.md) — admin 認可 (`ADMIN_API_TOKEN`) の生成・登録・rotation 手順 ([#560](https://github.com/SH11235/rshogi/issues/560))。
- [`viewer_access_control.md`](viewer_access_control.md) — viewer / spectate API の access control (Origin allowlist / kill-switch) 運用。
- [`lobby_design.md`](lobby_design.md) — LobbyDO + マッチングの詳細設計 (`/ws/lobby`、`MATCHED` 通知、queue 戦略)。
- [`lobby_e2e_runbook.md`](lobby_e2e_runbook.md) — Lobby マッチング対局を実機 staging で回す E2E 運用手順。
- [`clock_defaults.md`](clock_defaults.md) — 対局時計 (`CLOCK_KIND` / `CLOCK_PRESETS`) の設定ガイド。サポート方式・JSON schema・strict mode の挙動。

E2E 実機対局のシナリオ別手順 (平手 / 連続対局 / 切断再接続 / Buoy / 観戦 / 異常終局 / 時計違い) は repo 同梱の Skill `.claude/skills/csa-e2e-staging/SKILL.md` を参照。

## 関連 doc (このディレクトリ外)

- [`../csa-client.md`](../csa-client.md) — `csa_client` (CSA client 実装) の使い方。**Cloudflare Workers (WebSocket) 版への接続・公開対局場への参加手順**（接続先 URL / LOGIN 形式 / ロビー / 自前 Worker への接続）もここ。
- 各 crate 直下の Rust doc コメント (`rustdoc`) — 型 / 関数の最終契約はソース側を一次ソースとする。

私的対局の招待・参加・失敗時の再試行は [プロトコル参照](protocol-reference.md#921-workers-の私的対局)、
環境ごとの有効化と本人確認は [デプロイ運用](deployment.md#私的対局の運用設定) を参照してください。
