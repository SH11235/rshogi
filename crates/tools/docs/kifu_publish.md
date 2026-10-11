# kifu_publish

棋譜を rshogi の棋譜サーバー（`rshogi-csa-server-workers`）へ公開する CLI。公開した棋譜は
サーバーの一覧・検索・単局 API に載り、ビューアから閲覧できる。

| サブコマンド | 入力 | サーバーへの送り方 |
|---|---|---|
| `wdoor` | `csa_client` の棋譜ディレクトリ | 対局 ID だけを送り、サーバーが wdoor から棋譜を 1 回取得する |
| `jsonl` | `tournament` / `csa_client` の JSONL | wdoor 形式の CSA に変換して本文を送る |
| `csa` | CSA ファイル | 本文をそのまま送る |

## ビルドと実行

```bash
cargo build --release -p tools --bin kifu_publish
# → target/release/kifu_publish
```

## 共通の設定

| 指定 | 既定 | 説明 |
|---|---|---|
| `--api-base <URL>` | 環境変数 `RSHOGI_API_BASE` | API のベース URL。`/api/v1` まで含める |
| 環境変数 `RSHOGI_ADMIN_TOKEN` | （必須） | 管理トークン。引数では受け取らない（シェル履歴やプロセス一覧に残さないため）。ログやエラーにも出さない |
| `--interval-ms <N>` | `1000` | リクエストの間隔（ミリ秒） |
| `--dry-run` | off | サーバーへ送らず、送る内容を標準出力に出す。`RSHOGI_ADMIN_TOKEN` と `--api-base` は不要 |

```bash
export RSHOGI_API_BASE=https://<host>/api/v1
export RSHOGI_ADMIN_TOKEN=...   # サーバーの ADMIN_API_TOKEN
```

## wdoor: Floodgate 対局の取り込み

```bash
kifu_publish wdoor --records-dir ~/floodgate/records
kifu_publish wdoor --records-dir ~/floodgate/records --archive-dir ~/shogi-data/floodgate-archive
```

1. `--records-dir` 直下の `*.csa` から `$EVENT:` 行を読み、wdoor の対局 ID
   （`wdoor+<ゲーム名>+<先手>+<後手>+<開始時刻 14 桁>`）を集める。サブディレクトリは見ない。
   `_reconnect_fragment` 付きのファイルと、`$EVENT:` が wdoor の ID でないファイル（他サーバーでの
   対局）は対象外。
2. ID の重複を除いて昇順に並べ、1 局ずつ `POST {api-base}/admin/games/import-wdoor` を送る。
   サーバーに保存済みの局は wdoor へ取りに行かず `exists` になるので、同じディレクトリに対して
   何度実行してもよい。
3. サーバーが 404（wdoor に棋譜が無い）を返した局は、`--archive-dir` があればその配下の
   wdoor 版 CSA を `POST {api-base}/admin/games/upload` に `source: "wdoor"` で送る。探す場所は
   `<archive-dir>/YYYY/MM/DD/<id>.csa`、次に `<archive-dir>/<id>.csa`。ファイルの `$EVENT:` が
   対局 ID と一致しないときは送らずに失敗とする。`--archive-dir` が無い、またはファイルが
   見つからない局も失敗になる。

手元の `.csa`（`csa_client` の出力）は ID を集めるためだけに使い、本文は送らない。公開されるのは
両対局者の評価値・読み筋を含む wdoor 版の棋譜。

`--dry-run` では集めた対局 ID を 1 行 1 件で出す。

## jsonl: JSONL を CSA に変換して送る

```bash
# 1 局だけ
kifu_publish jsonl --input runs/sprt/pair-0-1__base-vs-test.jsonl --event SPRT-2026-10 --game-id 12
# ファイル内の終局した全局
kifu_publish jsonl --input ~/floodgate/records/jsonl/20261009_000001_RAMU_vs_Panda.jsonl --event local
# 送らずに変換結果だけ確認
kifu_publish jsonl --input game.jsonl --event test --dry-run
kifu_publish jsonl --input game.jsonl --event test --dry-run --out-dir ./csa-out
```

| 引数 | 説明 |
|---|---|
| `--input <FILE>` | `tournament` のペアファイルまたは `csa_client` の per-game JSONL（`.gz` 可） |
| `--event <LABEL>` | 一覧に出す大会名など。CSA の `$EVENT:` にも書く |
| `--game-id <N>` | この `game_id` の局だけを送る。省略時は `result` 行のある全局。`tournament` の `game_id` はペアファイルごとの連番 |
| `--out-dir <DIR>` | `--dry-run` 専用。標準出力の代わりに `<DIR>/<入力名>-<game_id>.csa` を書く |

`meta` / `move` / `result` 行から、対局ごとに次の内容の CSA を作り、
`POST {api-base}/admin/games/upload` に `source: "upload"` で送る。入力は 1 行ずつ読み、
`result` 行が現れた局から順に変換して送るので、局数の多いファイルでも保持するのは 1 局分。

- 対局者名: `move` 行の `engine`（先手・後手それぞれ最初の手）。片方が 1 手も指していない局は
  `meta.engine_cmd` のラベルから補う。
- 開始局面: 最初の手の `sfen_before`。
- 指し手・消費時間: `move_usi` を CSA の指し手に直し、`elapsed_ms` を秒に切り捨てて `T<秒>` に書く。
  終局用の擬似指し手（`resign` / `win` / `timeout` / `illegal` / `none`）と、時間切れで盤面に
  反映されなかった手は棋譜に入れない。
- 評価値・読み筋: `eval.score_cp`（詰みは `score_mate` の符号で ±100000）を先手視点に直し、
  `eval.pv` を CSA の指し手列にして `'** <評価値> <読み筋>` に書く。読み筋の先頭は実際の指し手
  そのもの（Floodgate へ送るコメントと同じ）。先頭が実際の指し手と違う読み筋は書かず、評価値だけ残す。
- 開始・終了時刻: `$START_TIME` は `meta.timestamp`（`tournament` では実行開始時刻）、
  `'$END_TIME:` は開始時刻 + 消費時間の合計。
- 持ち時間: `meta.settings` の `btime` / `binc` / `byoyomi` から `$TIME_LIMIT` を書く。すべて 0 なら省く。

出力の書式は [CSA の書式](#csa-の書式)のとおり。同じ入力からは常に同じ CSA ができ、サーバーの
棋譜 ID は本文から決まるので、送り直しても重複しない（`exists` になる）。

### 終局理由の対応

JSONL の `result.reason` を CSA の終局コマンドと `'summary:` の理由に直す。

| `reason` | 終局コマンド | `'summary:` の理由 |
|---|---|---|
| `resign` | `%TORYO` | `toryo` |
| `timeout`（tournament）/ `time_up`（csa_client） | `%TIME_UP` | `time up` |
| `illegal_move` | `%ILLEGAL_MOVE` | `illegal move` |
| `win`（tournament、宣言の move 行あり）/ `win_declaration`（csa_client） | `%KACHI` | `kachi` |
| `sennichite_perpetual_check`（tournament）/ `oute_sennichite`（csa_client） | `%ILLEGAL_MOVE` + `'OUTE_SENNICHITE` | `oute_sennichite` |
| `sennichite` | `%SENNICHITE` | `sennichite` |
| `jishogi` | `%JISHOGI` | `jishogi` |
| `max_moves` | `%MAX_MOVES` | `max_moves` |
| `abnormal` | `%CHUDAN` | `abnormal` |
| `adjudication_resign` / `adjudication_win`（tournament の評価値による裁定） | `%TORYO` | `toryo` |
| `win` / `lose`（csa_client、サーバーが理由行を送らなかった決着） | `%TORYO` | `toryo` |

次の対局は変換せず、その局を失敗として数える: `error` の付いた不成立局、`adjudication_draw`・
`no_bestmove`・`interrupted` など上の表にない理由、指し手が 1 手もない局、指し手を局面に適用
できない局。

## csa: CSA をそのまま送る

```bash
kifu_publish csa --input wcsc36-final.csa --event WCSC36
```

ファイルの内容を変更せず `POST {api-base}/admin/games/upload` に `source: "upload"` で送る。
サーバーは指し手を初期局面から再生できない棋譜を受け付けない。

## 出力と終了コード

1 局ごとに結果を出し、最後に件数を出す。失敗した局は標準エラーに記録して次の局へ進む。

```text
created wdoor-20261009000001-0123456789 wdoor+floodgate-300-10F+RAMU+Panda+20261009000001
exists  wdoor-20261009003000-abcdef0123 wdoor+floodgate-300-10F+RAMU+Other+20261009003000
failed  wdoor+floodgate-300-10F+Old+RAMU_TF+20260706123000: wdoor に棋譜がなく、... にも見つかりません
created=1 skipped=1 failed=1
```

| 表示 | 意味 |
|---|---|
| `created` | 新しく保存された |
| `exists`（件数は `skipped`） | サーバーに保存済みで何もしなかった（応答の `created` が `false`） |
| `failed` | 送信・変換に失敗した（HTTP ステータスと応答本文の先頭を表示） |

終了コードは、失敗が 0 件なら 0、1 件でもあれば 1。引数・環境変数の不備や入力ファイルが
読めない場合も 1。

## CSA の書式

`jsonl` が作る CSA と `csa_client` が保存する CSA は、wdoor の公開棋譜と同じ並びにそろえてある
（実装は `rshogi-csa-client` の `csa_writer` を共有）。

```text
V2.2
N+<先手名>
N-<後手名>
$EVENT:<イベント>
$START_TIME:2026/10/09 12:00:00
$TIME_LIMIT:5:00+10F
P1-KY-KE-GI-KI-OU-KI-GI-KE-KY
...
+
+7776FU
T2
'** 54 +7776FU -3334FU +2726FU
-3334FU
T0
'** 30 -3334FU +2726FU
%TORYO
'summary:toryo:<先手名> win:<後手名> lose
'$END_TIME:2026/10/09 12:01:04
```

- `'** <評価値> <読み筋>` は直前の指し手のコメントで、評価値は先手視点。評価値のない手には書かない。
- `'summary:<理由>:<先手名> <win|lose|draw>:<後手名> <win|lose|draw>` は勝敗が決まっている局だけに書く。
- `'summary:` の理由のうち、wdoor の実物で確認したのは `toryo` だけ。それ以外は shogi-server の
  表記に合わせたもので、実物との照合はしていない。

## テスト

```bash
cargo test -p tools --lib kifu_publish
```

対局 ID の収集、JSONL → CSA 変換（固定入力での出力一致）、HTTP 部分（ローカルのモックサーバーで
成功 / 保存済み / 404 → アーカイブ / 失敗）を検証する。
