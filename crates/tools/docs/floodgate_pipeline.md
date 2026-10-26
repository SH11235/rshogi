# floodgate_pipeline

Floodgate（`wdoor.c.u-tokyo.ac.jp`）の公開棋譜を取得し、NNUE 学習向けに
CSA → SFEN へ変換するパイプライン。サブコマンドは `fetch-ratings` /
`fetch-index` / `download` / `live-mirror` / `extract` の 5 つ。

通信は `reqwest`（rustls）で行う。Floodgate は http アクセスを https へ 301
誘導するため、既定 root は https を直接指す（`--root` に http を渡しても
サーバ側リダイレクトで動作する）。

## サブコマンド

### `fetch-ratings`

レーティングページから高レートプレイヤー名を取得し、`download` の事前フィルタ
（`--player-file`）に使うリストを出力する。

```bash
cargo run -p tools --bin floodgate_pipeline -- fetch-ratings --min-rating 3900 --out high_rated.txt
```

- `--url <URL>`: レーティングページ URL。**未指定なら直近の日付のページを自動取得**する。
  ページは日次生成の日付スタンプ付き（`.../shogi/x/rating/players-floodgate-YYYYMMDD.html`）で、
  サーバ（JST）基準の当日から最大 7 日遡って最初に取得できたものを採用する。
- `--min-rating <N>`: この値以上のプレイヤーを出力（既定 3900）。
- `--out <PATH>`: 出力先。1 行 1 プレイヤーで `名前<TAB>レート` 形式。

### `fetch-index`

棋譜インデックス `00LIST.floodgate` をダウンロードする。

```bash
cargo run -p tools --bin floodgate_pipeline -- fetch-index --out 00LIST.floodgate
```

- `--root <URL>`: Floodgate root（既定は https）。
- `--out <PATH>`: 出力先。

### `download`

インデックスに記載された CSA ファイルを並列ダウンロードする。インデックス内の
絶対パスは末尾の `/x/` 以降を相対パスとして `--out-dir` 配下へ配置する。

```bash
cargo run -p tools --bin floodgate_pipeline -- download \
  --index 00LIST.floodgate --date-from 2026-03-10 --player-file high_rated.txt --concurrency 16
```

- `--index <PATH>` / `--root <URL>` / `--out-dir <DIR>`。
- `--limit <N>`: ダウンロード数の上限（テスト用）。
- `--date-from` / `--date-to <YYYY-MM-DD>`: 日付フィルタ。
- `--player-file <PATH>`: いずれかの対局者が含まれる対局のみ取得（`fetch-ratings` の出力を渡す）。
- `--concurrency <N>`: 並列数（既定 8、`0` で CPU コア数）。

各 CSA は一時ファイルへ最後まで受信してから公開する。受信途中で失敗したファイルは残さず、
再実行時に取得し直す。既存ファイルは上書きしない。

### `live-mirror`

当日 (JST) の対局 CSA をローカル dir へミラーし続ける。`kifu_player --csa <dir> --live`
と組み合わせて、wdoor floodgate のほぼリアルタイム観戦に使う。両者を 1 コマンドに
まとめた wrapper 例が [docs/examples/floodgate/](../../../docs/examples/floodgate/README.md)
にある（`watch.sh` / `watch.ps1`）。

```bash
cargo run -p tools --bin floodgate_pipeline -- live-mirror \
  --out-dir /tmp/wdoor-live --watch RAMU_TF,Suisho --interval 10 --push
```

- `--out-dir <DIR>`: ミラー先（ファイル名は wdoor のまま。`kifu_player` の日付ソート・
  `date:` フィルタが末尾 14 桁タイムスタンプを認識する）。
- `--interval <SECS>`: 進行中対局の再取得間隔（既定 10 秒）。対局一覧（日次 autoindex）
  の確認は約 60 秒ごと（`--interval` がそれより長い場合はそのパス頻度。ペアリングは
  毎時 :00/:30 のため秒単位で追う意味は薄い）。
- `--watch <NAMES>`: 対局者名の部分一致フィルタ（カンマ区切り）。未指定は当日の全対局。
- `--push`: wdoor の MONITOR2 観戦接続を着手通知として使い、通知ごとに HTTP 公開 CSA を
  取得する。`--push` 時も棋譜本文の正本は HTTP で、MONITOR2 の snapshot 本文はミラーに使わない。
- `--login-name <NAME>`: `--push` 時の観戦ログイン名（既定 `rshogi-mirror`）。
- `--once`: 1 パスだけ実行して終了（動作確認用）。

挙動:

- wdoor の CSA は対局開始時に作られ**進行中も逐次追記**されるため、間隔ごとに毎回取得する
  （`Last-Modified` は秒精度で同秒追記を 304 に取り逃がすため条件付き GET は使わない。
  本文は数十 KB で転送コストは小さい）。追記のみで単調増加する性質から、同サイズなら
  ローカルへの書き込みを省く。
- **終局（`'$END_TIME:` コメント）を検出したファイルは以後取得しない**。過去の実行で
  ミラー済みの完全な棋譜も再取得しない。約 1 時間変化の無い進行中ファイルは中断対局と
  みなし追跡を止める（部分棋譜は「勝敗不明」として残る）。
- `--push` 時は `wdoor.c.u-tokyo.ac.jp:4081` へ観戦ログインし、`%%LIST` で進行中対局を
  見つける。HTTP で一度取得できた未終局の対象局にだけ `%%MONITOR2ON` を送り、MONITOR2 は
  「着手が来た」というドアベルとしてだけ使う。実際の CSA は既存と同じ HTTP 取得・tmp→rename
  書き込みで反映する。
- MONITOR2 のライブ broadcast には評価値コメント（`'**`）が含まれない。一方、HTTP 公開 CSA は
  broadcast 時点でコメント込みの最新状態になっているため、評価値付き観戦には HTTP を正本にする。
- `--push` 時も安全網として HTTP ポーリングを残す。TCP 接続断やログイン失敗時は警告を出し、
  `--interval` の従来ポーリングへフォールバックしながら 5 秒から最大 60 秒のバックオフで
  MONITOR2 再接続を続ける。接続中の安全網ポーリング間隔は `max(--interval, 60)` 秒。
- 書き込みは tmp→rename の全置換なので、`kifu_player --live` 側は常に完全な内容だけを読む。
- 取得は逐次 (1 リクエスト 30 秒タイムアウト)。`--watch` 無しで進行中対局が非常に多い日は
  1 パスが `--interval` を超えることがある (その場合は間隔を長めに、または `--watch` で絞る)。
- 停止は Ctrl-C。

手動スモーク:

```bash
cargo run -p tools --bin floodgate_pipeline -- live-mirror \
  --out-dir /tmp/wdoor-live --watch <対局者名の一部> --push
cargo run -p tools --bin kifu_player -- --csa /tmp/wdoor-live --live 5
```

`live-mirror.log` に `MONITOR2 接続完了` と `push 更新` が出て、着手後おおむね 1 秒以内に
ローカル CSA が評価値コメント込みで更新されればよい。TCP を切った場合も警告後に HTTP
ポーリングで更新が継続し、再接続後は push 更新へ戻る。

### `extract`

ローカルの CSA から学習用 SFEN を抽出する。

```bash
cargo run -p tools --bin floodgate_pipeline -- extract --min-rating 3900 --max-ply 32
```

- `--root <DIR>` / `--out <PATH>`（`-` で標準出力、`.gz` 対応）。
- `--mode <initial|all|nth>`、`--nth <手数,...>`、`--min-ply` / `--max-ply`。
- `--mirror-dedup`（水平ミラー正規化で重複排除）/ `--emit-mirror`。
- `--per-game-cap <N>`（1 棋譜あたりの最大抽出数）/ `--min-rating <N>`。

## 典型的な流れ

```bash
# 1. 高レートプレイヤーリスト
cargo run -p tools --bin floodgate_pipeline -- fetch-ratings --min-rating 3900 --out high_rated.txt
# 2. インデックス取得
cargo run -p tools --bin floodgate_pipeline -- fetch-index --out 00LIST.floodgate
# 3. CSA ダウンロード（日付・プレイヤーでフィルタ）
cargo run -p tools --bin floodgate_pipeline -- download --date-from 2026-03-10 --player-file high_rated.txt --concurrency 16
# 4. SFEN 抽出
cargo run -p tools --bin floodgate_pipeline -- extract --min-rating 3900 --max-ply 32
```

抽出はパスをソートし、128 棋譜ごとに並列解析して入力順に書き出す。全棋譜の SFEN を
一度に保持しないため、通常の抽出で保持する局面数は入力全体の局面数に比例しない。
パス一覧は棋譜数に比例する。ディレクトリ列挙に失敗すると、読めた部分だけで続行せずエラーで終了する。

`--mirror-dedup` は正規化した SFEN の集合をメモリに保持し、メモリ使用量は重複除去後の局面数に比例する。
数千万〜億局面の抽出ではこのフラグを付けず、必要なら抽出後の SFEN を外部ソートで重複除去する。
PSV に変換済みの教師データなら [psv_dedup_partition](psv_dedup_partition.md) でディスク方式の exact 重複除去を行える。

`extract --out` が入力 CSA と同じ実体（同一パス・hardlink）を指す場合や、出力自体が symlink の場合は拒否する。
