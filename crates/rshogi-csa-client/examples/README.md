# `rshogi-csa-client` 実行例

USI エンジンを CSA プロトコル対局サーバーに接続するブリッジ。TCP 経路と
WebSocket 経路の両方をサポートする。

詳細な設定リファレンスは [`docs/csa-client.md`](../../../docs/csa-client.md)、
Cloudflare Workers staging への実機 E2E は
[`docs/csa-server/staging-e2e.md`](../../../docs/csa-server/staging-e2e.md) を参照。

## クイックスタート (`--target` プリセット)

本リポ単一 Cloudflare アカウント (`sh11235.workers.dev`) の staging / production
Worker には TOML を書かずに 1 コマンドで接続できる。`--simple-engine` を併指定すると
staging の短秒読み (`BYOYOMI_MS=100`) でも完走する軽量設定 (MaterialLevel=1 /
USI_Hash=32 / margin_msec=0 / max_games=1 / ponder=false) が入る。

```bash
# 黒番 (staging)
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --room-id e2e-quickstart-1 \
  --handle alice \
  --color black \
  --simple-engine \
  --engine /path/to/your/rshogi-usi

# 別ターミナルで白番 (room_id を黒と一致させる)
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --room-id e2e-quickstart-1 \
  --handle bob \
  --color white \
  --simple-engine \
  --engine /path/to/your/rshogi-usi
```

production に繋ぎたい場合は `--target production` に差し替えるだけでよい
（production は `WS_ALLOWED_ORIGINS = ""` 運用前提でネイティブ経路 / Origin 欠落で
接続する）。本リポ以外の Cloudflare アカウントに deploy した Worker に繋ぎたい場合は
`--target` を使わず TOML / `--host` で URL を直接指定する。

`--simple-engine` を外すと TOML / `--hash` / `--ponder` / `--options K=V,K=V` の通常経路で
エンジン設定をフルコントロールできる。

## マッチングモード (`--lobby`)

`--lobby` を付けると LobbyDO (`/ws/lobby`) に接続して `<game_name>` 単位の待機
キューに入り、相補的な手番のペアが揃ったら自動で room_id を発番してその対局へ
接続するループに入る。`--max-games` まで対局を繰り返し、shutdown (Ctrl+C) で離脱
する。

```bash
# 黒番 (staging) で 5 局連続マッチング対局
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --lobby \
  --game-name rshogi-eval \
  --handle alice \
  --color black \
  --simple-engine \
  --engine /path/to/your/rshogi-usi \
  --max-games 5

# 別ターミナルで白番 (game_name を一致させる)
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --lobby \
  --game-name rshogi-eval \
  --handle bob \
  --color white \
  --simple-engine \
  --engine /path/to/your/rshogi-usi \
  --max-games 5
```

`<game_name>` は `[A-Za-z0-9_-]` / 1〜32 文字の制約あり。同 `game_name` 同士で
しかペアリングしない。`--lobby` は `--target` 経由でのみ動作する (LobbyDO の URL を
`wss://<subdomain>/ws/lobby` で組み立てる前提)。本リポ以外の Cloudflare アカウントの
Worker で動かしたい場合は TOML 直書きの host を `wss://<your-subdomain>/ws/lobby` に
向ける必要があり、現状の `--lobby` モードは未対応 (`--target staging|production` 必須)。

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
