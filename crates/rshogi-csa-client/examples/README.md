# `rshogi-csa-client` 実行例

USI エンジンを CSA プロトコル対局サーバーに接続するブリッジ。TCP 経路と
WebSocket 経路の両方をサポートする。

詳細な設定リファレンスは [`docs/csa-client.md`](../../../docs/csa-client.md)、
Cloudflare Workers staging への実機 E2E は
[`docs/csa-server/staging-e2e.md`](../../../docs/csa-server/staging-e2e.md) を参照。

## Workers staging × csa_client 実機 E2E

`csa_client_staging/scenarios/<scenario>/` 配下の `*.toml.example` をコピーして
`engine.path` / `host` URL / `id` を編集し、別ターミナルで黒・白を起動する。
シナリオ別の目的と手順は
[`docs/csa-server/staging-e2e.md`](../../../docs/csa-server/staging-e2e.md) の表を参照。

| シナリオ | ディレクトリ | 概要 |
| --- | --- | --- |
| A | `scenarios/A_basic_one_game/` | 平手 1 局完走（基本通電） |
| B | `scenarios/B_consecutive_games/` | 連続 5 対局（DO state 健全性） |
| C | `scenarios/C_reconnect/` | 切断→再接続（要 Floodgate features 有効化） |
| E | `scenarios/E_buoy/` | Buoy 中盤局面対局（要 ADMIN 権限） |
| G | `scenarios/G_clock_variants/` | 時計 kind 切替（A の TOML を流用） |

```bash
# 例: シナリオ A (平手 1 局完走)
cp crates/rshogi-csa-client/examples/csa_client_staging/scenarios/A_basic_one_game/black.toml.example \
   /tmp/A-black.toml
cp crates/rshogi-csa-client/examples/csa_client_staging/scenarios/A_basic_one_game/white.toml.example \
   /tmp/A-white.toml
# 各 .toml の engine.path / host URL / id を編集してから別ターミナルで起動。
cargo run -p rshogi-csa-client --release -- /tmp/A-black.toml
cargo run -p rshogi-csa-client --release -- /tmp/A-white.toml
```
