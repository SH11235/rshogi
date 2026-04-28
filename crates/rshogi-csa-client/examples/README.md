# `rshogi-csa-client` 実行例

USI エンジンを CSA プロトコル対局サーバーに接続するブリッジ。TCP 経路と
WebSocket 経路の両方をサポートする。

詳細な設定リファレンスは [`docs/csa-client.md`](../../../docs/csa-client.md)、
Cloudflare Workers staging への実機 E2E は
[`docs/csa-server/staging-e2e.md`](../../../docs/csa-server/staging-e2e.md) を参照。

## クイックスタート (`--target` プリセット)

本リポ単一 Cloudflare アカウント (`sh11235.workers.dev`) の staging / production
Worker には TOML を書かずに 1 コマンドで接続できる。エンジンには NNUE モデル付きの
本番想定構成を使う (例: `v82-400-layerstack.bin` 等の LayerStack NNUE モデルを `EvalFile` に渡す)。

```bash
# 黒番 (staging)
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --room-id e2e-quickstart-1 \
  --handle alice \
  --color black \
  --engine /path/to/your/rshogi-usi \
  --options "EvalFile=/path/to/your-nnue.bin,USI_Hash=256"

# 別ターミナルで白番 (room_id を黒と一致させる)
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --room-id e2e-quickstart-1 \
  --handle bob \
  --color white \
  --engine /path/to/your/rshogi-usi \
  --options "EvalFile=/path/to/your-nnue.bin,USI_Hash=256"
```

production に繋ぎたい場合は `--target production` に差し替えるだけでよい
（production は `WS_ALLOWED_ORIGINS = ""` 運用前提でネイティブ経路 / Origin 欠落で
接続する）。本リポ以外の Cloudflare アカウントに deploy した Worker に繋ぎたい場合は
`--target` を使わず TOML / `--host` で URL を直接指定する。

エンジンビルドの feature 選定は `bullet-shogi/docs/experiments/` の各モデル仕様 +
`.claude/skills/selfplay/SKILL.md` の features 対応表を参照。

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
  --engine /path/to/your/rshogi-usi \
  --options "EvalFile=/path/to/your-nnue.bin,USI_Hash=256" \
  --max-games 5

# 別ターミナルで白番 (game_name を一致させる)
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --lobby \
  --game-name rshogi-eval \
  --handle bob \
  --color white \
  --engine /path/to/your/rshogi-usi \
  --options "EvalFile=/path/to/your-nnue.bin,USI_Hash=256" \
  --max-games 5
```

`<game_name>` は `[A-Za-z0-9_-]` / 1〜32 文字の制約あり。同 `game_name` 同士で
しかペアリングしない。`--lobby` は `--target` 経由でのみ動作する (LobbyDO の URL を
`wss://<subdomain>/ws/lobby` で組み立てる前提)。本リポ以外の Cloudflare アカウントの
Worker で動かしたい場合は TOML 直書きの host を `wss://<your-subdomain>/ws/lobby` に
向ける必要があり、現状の `--lobby` モードは未対応 (`--target staging|production` 必須)。

## JSONL 出力モード — `tools::analyze_selfplay` で集計

`--jsonl-out <DIR>` を付けて起動すると、対局完了ごとに analyze_selfplay 互換の JSONL を
`<DIR>/<datetime>_<sente>_vs_<gote>.jsonl` として書き出す。サーバーへ送信するわけでは
なく、完全にローカル CLI 解析専用の opt-in 機能 (既定 OFF)。

スキーマは selfplay (`tools/src/bin/tournament.rs`) の出力と同じ `meta` / `move` /
`result` の 3 種類で、`move.eval` は `score_cp` / `score_mate` / `depth` / `seldepth` /
`nodes` / `time_ms` / `nps` / `pv` を含む。`engine` フィールドは CSA 上の player 名
(`sente_name` / `gote_name`) と一致するため、selfplay の per-engine 集計と同じツールを
そのまま流用できる。

```bash
# 1. CSA 経由対局を JSONL 付きで実行（--target staging の例）
mkdir -p runs/csa-jsonl
cargo run -p rshogi-csa-client --release -- \
  --target staging \
  --room-id e2e-jsonl-1 \
  --handle alice \
  --color black \
  --engine /path/to/your/rshogi-usi \
  --options "EvalFile=/path/to/your-nnue.bin,USI_Hash=256" \
  --jsonl-out runs/csa-jsonl \
  --max-games 5

# 2. selfplay と同じツールで集計
cargo run -p tools --release --bin analyze_selfplay -- runs/csa-jsonl/*.jsonl

# JSON で受け取りたい場合
cargo run -p tools --release --bin analyze_selfplay -- --json runs/csa-jsonl/*.jsonl
```

TOML 設定の `[record]` セクションでも指定可能:

```toml
[record]
enabled = true
dir = "./records"
jsonl_out = "./runs/csa-jsonl"
```

注意点:

- 1 対局 = 1 JSONL ファイル。複数局を回した場合は glob 展開でまとめて
  analyze_selfplay に渡す。
- 相手エンジンのバイナリパス・USI options は CSA プロトコル上で得られないため
  `path_white` / `path_black` の片側は `remote:<player_name>` 形式の placeholder が入る。
  per-engine 集計に必要な `label_*` は `sente_name` / `gote_name` を使うので
  `winner` 判定はそのまま動く。
- 相手手番の `move` 行は `eval` を持たない（USI info を観測できないため）。
  集計対象は自エンジンの `engine_timing` のみ意味を持つ。

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
