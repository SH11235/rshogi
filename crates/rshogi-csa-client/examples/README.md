# `rshogi-csa-client` 実行例

USI エンジンを CSA プロトコル対局サーバーに接続するブリッジ。TCP 経路と
WebSocket 経路の両方をサポートする。

詳細な設定リファレンスは [`docs/csa-client.md`](../../../docs/csa-client.md)、
Cloudflare Workers staging への実機 E2E は
[`docs/csa-server/staging-e2e.md`](../../../docs/csa-server/staging-e2e.md) を参照。

## Workers staging × csa_client 実機 E2E

`csa_client_staging/` 配下の `*.toml.example` をコピーして
`engine.path` / `host` URL / `id` を編集し、別ターミナルで黒・白を起動する。

```bash
cp crates/rshogi-csa-client/examples/csa_client_staging/staging-black.toml.example \
   /tmp/staging-black.toml
cp crates/rshogi-csa-client/examples/csa_client_staging/staging-white.toml.example \
   /tmp/staging-white.toml
# 各 .toml の engine.path / host URL / id を編集してから別ターミナルで起動。
cargo run -p rshogi-csa-client --release -- /tmp/staging-black.toml
cargo run -p rshogi-csa-client --release -- /tmp/staging-white.toml
```
