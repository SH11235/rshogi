# `rshogi-csa-server-tcp` 運用ガイド

本リポジトリ同梱の **TCP 対局サーバー** `rshogi-csa-server-tcp` を建てて、CSA-over-TCP で
複数の USI エンジンを対局させるための起動・設定ガイド。`crates/rshogi-csa-server`
（transport 非依存のコアロジック）の TCP フロントエンドで、ローカル／LAN／クラウドで
借りたマシン上に常駐させて使う。

クライアント（`csa_client`）側の設定は [`../csa-client.md`](../csa-client.md) を参照。
Cloudflare Workers（WebSocket）版を運用する場合は [`deployment.md`](deployment.md)。

## 1. 位置づけ

| | `rshogi-csa-server-tcp`（本ガイド） | Workers 版 | Ruby shogi-server |
|---|---|---|---|
| transport | 生 TCP | WebSocket（Cloudflare 上） | 生 TCP |
| プレイヤ | `players.toml` に登録（handle + password） | handle ベース | 匿名（password = ゲーム名） |
| LOGIN id | `<handle>+<game_name>+<color>` | 同左 | `<name>`（password にゲーム名） |
| 用途 | 自前マシンでの対局・評価を常駐運用 | サーバーレス／Web 公開 | 互換リファレンス実装 |

shogi-server とは LOGIN 規約・プレイヤ管理が異なるため、`csa_client` から本サーバーへ
繋ぐときは下記の `<handle>+<game_name>+<color>` 形式を使う。

## 2. クイックスタート（最小構成）

対局するハンドルとパスワードを `players.toml` に登録する:

```toml
# players.toml
[players.alice]
password = "alicepw"

[players.bob]
password = "bobpw"
```

サーバーを起動する（`--players` のみ必須。`--bind` / `--kifu-dir` は既定値あり）:

```bash
cargo run -p rshogi-csa-server-tcp --release -- \
  --bind 0.0.0.0:4081 \
  --kifu-dir ./kifu \
  --players ./players.toml
```

起動ログに `listening bind=...` → `ready` が出れば待受開始。`--clock-presets-toml` を
渡さない限り、全 `game_name` に global clock（既定 `countdown` 600 秒 + 10 秒読み）が
適用される（後方互換モード）。

クライアントを 2 つ接続する（詳細は [`../csa-client.md`](../csa-client.md)）。LOGIN id は
`<handle>+<game_name>+<color>`、`password` は `players.toml` の値。**同じ `<game_name>` +
逆の `<color>`** でマッチングが成立する:

```toml
# 先手側 client
[server]
host = "192.168.1.100"   # サーバーの IP
port = 4081
id = "alice+casual-600-10+black"
password = "alicepw"
floodgate = false
```

```toml
# 後手側 client
[server]
host = "192.168.1.100"
port = 4081
id = "bob+casual-600-10+white"
password = "bobpw"
floodgate = false
```

## 3. `players.toml`

`[players.<handle>]` テーブルを handle ごとに並べる。パスワード照合は **handle 単位**で
行われ（LOGIN id の `<handle>` 部分で lookup）、登録の無い handle は `LOGIN:incorrect`。

| フィールド | 必須 | 既定 | 説明 |
|---|---|---|---|
| `password` | ✅ | — | 平文。`csa_client` の `password` と一致させる |
| `rate` | | `1500` | レート初期値（`--players-yaml` 併用時のみ意味を持つ） |
| `wins` | | `0` | 勝数初期値（同上） |
| `losses` | | `0` | 負数初期値（同上） |

`--players-yaml` を併用しない場合、`rate` / `wins` / `losses` はインメモリ保持で
再起動時に失われる（開発用）。永続化は §6 を参照。

## 4. マッチングと LOGIN

- クライアントは `LOGIN <handle>+<game_name>+<color> <password>` で接続する。
- `<color>` は `black` / `white`（先手 / 後手）。**両者が逆の色を明示**する必要がある
  （色の自動・ランダム割当は未対応）。
- `<game_name>` が一致し色が相補的な 2 者が待機プールに揃った時点でペアリングされる
  （既定 `DirectMatchStrategy`）。`<game_name>` はマッチングのプール名を兼ねる。

### 4.1 受信行の上限

TCP サーバーが受け取る1行の上限は **64 KiB（65,536 byte）**。UTF-8 のバイト数で数え、
末尾の LF は除き、CR は含む。CRLF で送る場合、本文は最大65,535 byteとなる。
LOGIN 前後を問わず適用され、`%%CHAT` や手列を1行で送る `%%SETBUOY` も対象になる。
上限を超えると、改行や切断を待たずにその接続を終了する。超過した行を分割して別の
コマンドとして受理することはない。

## 5. 時計設定

### 5.1 global clock（既定・後方互換）

`--clock-presets-toml` を渡さない場合、CLI で 1 つの clock を指定し全 `game_name` に適用する:

| フラグ | 既定 | 説明 |
|---|---|---|
| `--clock-kind` | `countdown` | `countdown` / `fischer` / `stopwatch` |
| `--total-time-sec` | `600` | 秒読み / Fischer の持ち時間（秒） |
| `--byoyomi-sec` | `10` | 秒読み秒、または Fischer の増分（秒） |
| `--total-time-min` | `10` | StopWatch の持ち時間（分） |
| `--byoyomi-min` | `1` | StopWatch の秒読み（分） |
| `--margin-ms` | `1500` | 通信マージン（ミリ秒） |

### 5.2 `--clock-presets-toml`（strict mode）

`game_name` ごとに別々の時計を使いたい場合は `[[preset]]` を宣言する。**1 件でも宣言すると
strict mode** になり、未登録 `game_name` の LOGIN は `LOGIN:incorrect unknown_game_name` で
拒否される。

```toml
# clock_presets.toml
[[preset]]
game_name = "byoyomi-600-10"
kind = "countdown"
total_time_sec = 600
byoyomi_sec = 10

[[preset]]
game_name = "fischer-300-10F"
kind = "fischer"
total_time_sec = 300
increment_sec = 10
```

`kind` は `countdown` / `countdown_msec` / `fischer` / `stopwatch`。各方式のパラメータ名と
セマンティクスは [`clock_defaults.md`](clock_defaults.md) を参照（Workers 版と同じ
`ClockSpec` を共有する）。

## 6. Floodgate 運用機能（`--allow-floodgate-features` ゲート）

以下は本フラグを立てたときのみ有効化される。安全側の既定として、フラグ無しで
これらのオプションを渡すと起動が失敗する。

| オプション | 機能 |
|---|---|
| `--players-yaml <PATH>` | Ruby shogi-server 互換 `players.yaml` にレート・勝敗・最終対局を atomic write で書き戻し、再起動後も永続 |
| `--floodgate-schedule-toml <PATH>` | **定刻ラウンドマッチメイク**（§7） |
| `--floodgate-history-jsonl <PATH>` | 終局ごとに 1 行（開始時刻・ペア・結果・勝者）を append |
| `--duplicate-login-evict-old` | 同名ログイン重複時に旧セッションを evict（既定は新接続を拒否）。対局進行中は evict されない |

## 7. 定刻ラウンドマッチメイク（`[[schedules]]`）

`--floodgate-schedule-toml` で、特定の `game_name` を **UTC の曜日・時刻に発火**させ、
その時点で待機しているプレイヤをまとめてペアリングする（本家 Floodgate 相当の
スケジュール対局）。`--allow-floodgate-features` が必須。

```toml
# schedules.toml
[[schedules]]
game_name = "floodgate-600-10"
weekday = "Sat"        # Mon / Tue / Wed / Thu / Fri / Sat / Sun（UTC）
hour = 21              # 0..=23（UTC）
minute = 0             # 0..=59
pairing_strategy = "direct"
```

発火時に当該 `game_name` の待機プール全員を取り出し、ペアを計算 → 対局起動 →
余った待機者はプールに戻す。clock はこの構造体には持たせず、`game_name` を鍵に
clock preset（§5.2）/ global clock から解決する（同じ `game_name` を曜日違いで
複数 `[[schedules]]` に並べれば曜日別運用になる）。

`pairing_strategy` は次の 2 値を受理する（未知の値は起動時 Err）:

- `"direct"`: 相補手番の最初の 1 組を返す `DirectMatchStrategy`。
- `"least_diff"`: レート差・連戦ペナルティを最小化する `LeastDiffPairingStrategy`（既定試行 300 回）。

## 8. 駒落ち初期局面（`--handicap-toml`）

`[[handicap]]` で `game_name` ごとに開始 SFEN を差し替える。

```toml
# handicap.toml
[[handicap]]
game_name = "hirate"
sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
```

`sfen` は盤面 / 手番 / 持駒 / 手数の 4 フィールドを最低限満たす必要があり、満たさない場合は
起動時に fail-fast する。

## 9. 棋譜出力

`--kifu-dir`（既定 `./kifu`）に CSA 棋譜と `00LIST` が保存される。

## 10. その他のオプション

| オプション | 既定 | 説明 |
|---|---|---|
| `--max-moves` | `256` | 最大手数 |
| `--agree-timeout-sec` | `300` | `AGREE` 受信の最大待機（エンジン起動待ちを許容） |
| `--shutdown-grace-sec` | `60` | SIGINT/SIGTERM 後に進行中対局の完了を待つ秒数 |
| `--challenge-ttl-sec` | `3600` | 私的対局 `%%CHALLENGE` トークンの TTL（秒） |
| `--admin-handle <HANDLE>` | （空） | `%%SETBUOY` / `%%DELETEBUOY` を許可する handle（複数可）。空だとブイ登録は全拒否 |
| `--metrics-bind <ADDR>` | （無効） | Prometheus 互換メトリクスを expose する HTTP listener の bind 先 |

## 11. 関連 doc

- [`../csa-client.md`](../csa-client.md) — `csa_client`（クライアント）の使い方。
- [`protocol-reference.md`](protocol-reference.md) — 受理する CSA / x1 拡張コマンドの一覧。
- [`clock_defaults.md`](clock_defaults.md) — clock 方式（`ClockSpec`）の詳細。
- [`deployment.md`](deployment.md) — Cloudflare Workers（WebSocket）版の運用 runbook。
- 各型 / 関数の最終契約はソース（`crates/rshogi-csa-server-tcp` / `crates/rshogi-csa-server`）を一次ソースとする。
