# CSA対局クライアント 要件定義

## 概要

USIエンジン（rshogi）をCSAプロトコル対局サーバー（floodgate等）にCLIから接続するブリッジツール。
将棋所などのGUIを介さず、バックグラウンドで連続対局を実行できるようにする。

**バイナリ名**: `csa_client`
**配置**: `crates/tools/src/bin/csa_client.rs`

## 参考実装

| ツール | 言語 | 採用ポイント |
|--------|------|------------|
| `usiToCsa.rb` (TadaoYamaoka/shogi-server) | Ruby | CSAプロトコル処理の実績・ponder実装の堅牢さ・シンプルな設計 |
| `usi-csa-bridge` (sunfish-shogi/shogihome) | TypeScript | 設定ファイル方式・自動再接続・棋譜保存・ログ設計の充実 |

---

## 機能要件

### F1: CSAプロトコル通信

- TCP接続によるCSAサーバーとのテキスト行ベース通信
- CSA v1.2.1 準拠 + floodgate拡張（評価値・PVコメント送信）
- プロトコルフロー:
  1. `LOGIN <id> <password>` → `LOGIN:... OK` 確認
  2. `Game_Summary` 受信・解析（手番、時間設定、途中局面）
  3. `AGREE` 送信 → `START` 受信で対局開始
  4. 指し手の送受信（`+7776FU,T3` 形式）
  5. 終局コマンド受信（`#WIN`, `#LOSE`, `#DRAW`, `#CENSORED`, `#CHUDAN`）
  6. `LOGOUT` 送信
- 特殊手の送信: `%TORYO`（投了）, `%KACHI`（入玉宣言勝ち）
- サーバーは floodgate に限定せず、任意の CSAサーバー（host/port指定）に対応

### F2: USIエンジン管理

- 子プロセスとしてUSIエンジンを起動（stdin/stdout パイプ通信）
- 起動シーケンス: `usi` → `usiok` → `setoption` → `isready` → `readyok`
- 対局中: `usinewgame` → `position` + `go` → `bestmove` のループ
- 終局: `gameover win/lose/draw` → （再利用 or `quit`）
- USIオプション設定: TOML設定またはCLIから `key=value` 形式で指定
- エンジン起動タイムアウト（デフォルト30秒）

### F3: CSA手 ⇔ USI手 変換

- 既存の `common/csa.rs` のCSAパーサを基盤として活用
- CSA→USI: `+7776FU` → `7g7f`, `+0076FU` → `P*7f`, `+8822UM` → `8h2b+`
- USI→CSA: 逆変換。盤面状態から駒種を解決
- 内部盤面を保持して合法手検証に利用

### F4: 時間管理

- `Game_Summary` から `Total_Time`, `Byoyomi`, `Increment` を解析
- 消費時間トラッキング: サーバーからの `T<秒>` で更新
- USIエンジンへの `go` コマンドで適切な時間パラメータを送信:
  - 秒読み: `go btime <bt> wtime <wt> byoyomi <by>`
  - フィッシャー: `go btime <bt> wtime <wt> binc <inc> winc <inc>`
- マージン設定: 通信遅延を考慮した安全マージン（デフォルト2500ms）を秒読みから差し引く

### F5: Ponder

- `bestmove <move> ponder <ponder_move>` 受信時にponder開始
- 自手をサーバーに送信後、`position ... <ponder_move>` + `go ponder` をエンジンに送信
- 相手の指し手受信時:
  - **予測的中**: `ponderhit` を送信、エンジンは探索継続
  - **予測外れ**: `stop` を送信 → `bestmove` を待って破棄 → 正しい局面で `go` 再発行
- ponder中の時間パラメータ: 自手の推定消費時間を差し引いて計算

### F6: Keep-alive

- **TCP keepalive**: ソケットレベルの keepalive 有効化（`SO_KEEPALIVE`）
- **CSAレベル ping**: 設定された間隔（デフォルト60秒）でサーバーに空行を送信
  - 最後の送受信からの経過時間で判定
  - CSAプロトコル規定により30秒以上の間隔を強制

### F7: 自動再接続・連続対局

- 対局終了後、自動的に再ログインして次の対局を待機
- 最大対局回数の設定（0 = 無制限）
- エラー時のリトライ: 指数バックオフ付き再接続（初回10秒、最大15分）
- 毎局エンジン再起動オプション（メモリリーク対策、デフォルトoff）
- graceful shutdown: SIGINT/SIGTERM で現在の対局完了後に終了（対局中なら投了して終了）

### F8: 棋譜保存

- 対局ごとにCSA形式の棋譜ファイルを自動保存
  - ファイル名テンプレート: `{datetime}_{sente}_vs_{gote}.csa`
  - floodgateコメント（評価値・PV）も含めて記録
- SFEN局面列の出力（学習データ生成パイプラインとの連携用）
  - 各局面のSFEN + 指し手 + 評価値をタブ区切りで出力
- 保存ディレクトリは設定で指定可能

### F9: ログ出力

- 3カテゴリの構造化ログ:
  - **CSA通信**: `[CSA] > LOGIN ...` / `[CSA] < +7776FU,T3` （パスワードはマスク）
  - **USI通信**: `[USI] > position startpos moves ...` / `[USI] < bestmove 7g7f`
  - **アプリケーション**: `[APP] 対局開始: game_id=...`
- ログレベル: `error`, `warn`, `info`, `debug`, `trace`
- 出力先: stdout + ファイル（日次ローテーション）
- タイムスタンプ: マイクロ秒精度

### F10: Floodgate拡張

- floodgateモード時、指し手と共に評価値・PVをコメント送信
  - 形式: `'* <評価値> <PV手順（CSA形式）>`
  - 評価値は先手視点に正規化（後手番なら符号反転）
  - mate スコアは `+/-100000` に変換

---

## 設定方式

### TOML設定ファイル（メイン）

```toml
[server]
host = "wdoor.c.u-tokyo.ac.jp"
port = 4081
id = "rshogi_v1"
password = "secret"
floodgate = true            # floodgate拡張（評価値コメント送信）

[server.keepalive]
tcp = true                  # TCP SO_KEEPALIVE
ping_interval_sec = 60      # CSAレベル空行ping間隔

[engine]
path = "/path/to/rshogi"
startup_timeout_sec = 30

[engine.options]
USI_Hash = 1024
USI_Ponder = true
Threads = 4
# エンジン固有オプションを自由に追加可能

[time]
margin_msec = 2500          # 秒読みマージン

[game]
max_games = 0               # 0 = 無制限
restart_engine_every_game = false
ponder = true

[retry]
initial_delay_sec = 10
max_delay_sec = 900         # 15分

[record]
enabled = true
dir = "./records"
# テンプレート変数: {datetime}, {game_id}, {sente}, {gote}
filename_template = "{datetime}_{sente}_vs_{gote}"
save_csa = true
save_sfen = true            # SFEN局面列出力

[log]
level = "info"              # error, warn, info, debug, trace
dir = "./logs"
stdout = true
```

### CLIオプション（オーバーライド用）

```
csa_client [OPTIONS] [CONFIG_FILE]

引数:
  CONFIG_FILE               TOML設定ファイルのパス

オプション (設定ファイルの値をオーバーライド):
  --host <HOST>             CSAサーバーホスト名
  --port <PORT>             CSAサーバーポート番号
  --id <ID>                 ログインID
  --password <PASSWORD>     パスワード
  --engine <PATH>           USIエンジンのパス
  --hash <MB>               USI_Hashサイズ
  --ponder                  ponder有効化
  --floodgate               floodgateモード
  --keep-alive <SEC>        keep-alive間隔(秒)
  --margin-msec <MSEC>      秒読みマージン(ms)
  --max-games <N>           最大対局数 (0=無制限)
  --log-level <LEVEL>       ログレベル
  --record-dir <DIR>        棋譜保存ディレクトリ
  --options <K=V,K=V,...>   USIエンジンオプション
```

環境変数でも設定可能（`CSA_HOST`, `CSA_ID`, `CSA_PASSWORD` 等）。
優先順位: CLIオプション > 環境変数 > TOML設定ファイル > デフォルト値

---

## 非機能要件

### NF1: 依存関係

- `tokio`: 非同期TCP通信（サーバー通信とエンジン通信の並行処理に必要）
- `clap`: CLIオプションパーサ（既存ツールで使用実績あり）
- `toml` / `serde`: 設定ファイル読み込み
- `tracing` + `tracing-subscriber`: 構造化ログ
- 既存の `common/csa.rs` を拡張してCSA手⇔USI手変換を追加

### NF2: エラー耐性

- サーバー切断・エンジンクラッシュ時に panic せず `Result` で伝播
- 対局中のエラーは投了してから再接続
- ネットワーク一時障害はリトライで吸収

### NF3: リソース効率

- ホットパスでのヒープ割り当て回避（CSA手変換等はスタックバッファ使用）
- エンジンプロセスの適切なクリーンアップ（孤児プロセス防止）

---

## 実装フェーズ

### Phase 1: 最小動作版
- CSAログイン〜対局〜終局の基本フロー
- USIエンジン起動・通信
- CSA⇔USI手変換
- 時間管理
- CLIオプション + TOML設定
- ログ出力（stdout）

### Phase 2: 運用品質
- Ponder
- Keep-alive
- 自動再接続・連続対局
- 棋譜保存（CSA + SFEN）
- Floodgate拡張コメント
- ログファイル出力・ローテーション
- Graceful shutdown

### Phase 3: 改善（必要に応じて）
- Early Ponder（やねうら王拡張）
- 対局統計のリアルタイム表示（勝敗数、レーティング推定）
- `analyze_selfplay` との統合
