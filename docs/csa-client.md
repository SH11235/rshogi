# CSA対局クライアント (`csa_client`)

USIエンジンをCSAプロトコル対局サーバー（floodgate等）にCLIから接続するツール。
将棋所などのGUIを介さず、バックグラウンドで連続対局を実行できる。

## クイックスタート

```bash
# 1. 設定ファイルを用意（csa_client_example.toml をコピーして編集）
cp csa_client_example.toml my_config.toml
# → server.id, server.password, engine.path を書き換える

# 2. 実行
cargo run -p tools --bin csa_client --release -- my_config.toml
```

## 設定

TOML設定ファイルで管理する。`csa_client_example.toml` にデフォルト値付きの全設定項目がある。

### 設定の優先順位

CLIオプション > 環境変数 > TOML設定ファイル > デフォルト値

### CLIオプション

設定ファイルの値を部分的にオーバーライドできる:

```bash
cargo run -p tools --bin csa_client --release -- config.toml \
  --id my_engine \
  --max-games 10 \
  --ponder true \
  --hash 2048 \
  --options "Threads=8,EvalFile=/path/to/nn.bin"
```

主なオプション:

| オプション | 説明 |
|-----------|------|
| `--host` | CSAサーバーホスト名 |
| `--port` | ポート番号 |
| `--id` | ログインID |
| `--password` | パスワード |
| `--engine` | USIエンジンのパス |
| `--hash` | USI_Hash (MB) |
| `--ponder` | ponder 有効/無効 |
| `--floodgate` | floodgateモード（評価値コメント送信） |
| `--keep-alive` | keep-alive間隔（秒） |
| `--margin-msec` | 秒読みマージン（ms） |
| `--max-games` | 最大対局数（0=無制限） |
| `--log-level` | ログレベル (error/warn/info/debug/trace) |
| `--record-dir` | 棋譜保存ディレクトリ |
| `--options` | USIオプション (K=V,K=V,...) |

### 環境変数

`CSA_HOST`, `CSA_PORT`, `CSA_ID`, `CSA_PASSWORD` が使える。
シェルスクリプトでパスワードを設定ファイルに書きたくない場合に便利。

## 主な設定項目

### `[server]` — 接続先

```toml
[server]
host = "wdoor.c.u-tokyo.ac.jp"  # floodgate
port = 4081
id = "rshogi_v1"
password = "your_password"
floodgate = true  # 評価値・PVコメントを送信
```

### `[engine]` — USIエンジン

```toml
[engine]
path = "./target/release/rshogi-usi"
startup_timeout_sec = 30

[engine.options]
USI_Hash = 1024
Threads = 4
```

`[engine.options]` にはUSIエンジンが対応する任意のオプションを書ける。

### `[time]` — 時間管理

```toml
[time]
margin_msec = 2500  # 通信遅延を考慮した安全マージン
```

秒読みからこの値を差し引いてエンジンに渡す。ネットワーク越しの対局では大きめに設定。

### `[game]` — 対局設定

```toml
[game]
max_games = 0       # 0 = 無制限に連続対局
ponder = true       # 相手手番中の先読み
restart_engine_every_game = false  # メモリリーク対策
```

### `[record]` — 棋譜保存

```toml
[record]
enabled = true
dir = "./records"
filename_template = "{datetime}_{sente}_vs_{gote}"
save_csa = true   # CSA形式
save_sfen = true  # SFEN局面列（学習データ生成用）
```

テンプレート変数: `{datetime}`, `{game_id}`, `{sente}`, `{gote}`

## 使い方の例

### floodgate で連続対局

```toml
[server]
host = "wdoor.c.u-tokyo.ac.jp"
port = 4081
id = "rshogi_test"
password = "any_string_here"
floodgate = true

[game]
max_games = 0
ponder = true
```

```bash
# バックグラウンド実行
nohup cargo run -p tools --bin csa_client --release -- config.toml > csa.log 2>&1 &
```

Ctrl+C (SIGINT) で現在の対局完了後にgracefulに終了する。

### LAN内の自前サーバーで対局

[shogi-server](https://github.com/TadaoYamaoka/shogi-server) をサーバーとして起動:

```bash
# サーバー側（Ruby必要）
cd shogi-server
./shogi-server test 4081
```

2台のマシンで `csa_client` を接続。**パスワードに同じゲーム名を指定**するとマッチングされる:

```toml
# ゲーム名の形式: <名前>-<持ち時間秒>-<秒読み秒>
# 例: "match-300-10" → 持ち時間300秒 + 秒読み10秒
# 末尾F でフィッシャー: "match-300-10F" → 300秒 + 1手10秒加算
[server]
host = "192.168.1.100"
port = 4081
id = "engine_a"
password = "match-300-10"
floodgate = false
```

## 出力

### ログ

```
[2026-03-28T12:00:00.123Z INFO] CSA対局クライアント起動
[2026-03-28T12:00:00.456Z INFO] [CSA] ログイン成功: rshogi_v1
[2026-03-28T12:00:30.789Z INFO] [CSA] 対局開始: START:game123
[2026-03-28T12:10:15.012Z INFO] 対局 #1 結果: Win | 通算: 1勝 0敗 0分
```

`--log-level debug` で CSA/USI 通信の全行が表示される。

### 棋譜ファイル

`records/` ディレクトリに対局ごとに保存:
- `20260328_120030_rshogi_v1_vs_opponent.csa` — CSA形式（評価値コメント付き）
- `20260328_120030_rshogi_v1_vs_opponent.sfen` — SFEN局面列（タブ区切り: SFEN, 指し手, 評価値）
