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
| `--api-base <URL>` | 環境変数 `RSHOGI_API_BASE` | API のベース URL。`/api/v1` まで含める。`https://` のみ（下記） |
| 環境変数 `RSHOGI_ADMIN_TOKEN` | （必須） | 管理トークン。引数では受け取らない（シェル履歴やプロセス一覧に残さないため）。ログやエラーにも出さない |
| `--interval-ms <N>` | `1000` | リクエストの間隔（ミリ秒） |
| `--dry-run` | off | サーバーへ送らず、送る内容だけを標準出力に出す（件数などは標準エラー）。`RSHOGI_ADMIN_TOKEN` と `--api-base` は不要 |

```bash
export RSHOGI_API_BASE=https://<host>/api/v1
export RSHOGI_ADMIN_TOKEN=...   # サーバーの ADMIN_API_TOKEN
```

管理トークンを送るので、接続先は次のように制限している。

- `--api-base` は `https://` で指定する。`http://` は `localhost` / `127.0.0.1` / `[::1]`
  （ローカルの開発サーバー）だけ受け付け、それ以外はトークンが平文で流れるので起動時にエラーにする。
- リダイレクトには追従しない。3xx が返った局は失敗になる（別ホストへトークンを送らない）。

## wdoor: Floodgate 対局の取り込み

```bash
kifu_publish wdoor --records-dir ~/floodgate/records
kifu_publish wdoor --records-dir ~/floodgate/records --archive-dir ~/shogi-data/floodgate-archive
```

1. `--records-dir` 直下の `*.csa` から `$EVENT:` 行を読み、wdoor の対局 ID
   （`wdoor+<ゲーム名>+<先手>+<後手>+<開始時刻 14 桁>`）を集める。サブディレクトリは見ない。
   `_reconnect_fragment` 付きのファイルと、`$EVENT:` が wdoor の ID でないファイル（他サーバーでの
   対局）は対象外。読めないファイルは警告を出して飛ばす（`$EVENT:` 行はバイト列のまま探すので、
   Shift_JIS の棋譜が混ざっていても ID は取り出せる）。
2. ID の重複を除いて昇順に並べ、1 局ずつ `POST {api-base}/admin/games/import-wdoor` を送る。
   サーバーに保存済みの局は wdoor へ取りに行かず `exists` になるので、同じディレクトリに対して
   何度実行してもよい。
3. サーバーが「wdoor に棋譜が無い」と返した局は、`--archive-dir` があればその配下の
   wdoor 版 CSA を `POST {api-base}/admin/games/upload` に `source: "wdoor"` で送る。探す場所は
   `<archive-dir>/YYYY/MM/DD/<id>.csa`、次に `<archive-dir>/<id>.csa`。ファイルの `$EVENT:` が
   対局 ID と一致しないとき、UTF-8 でないときは送らずに失敗とする。`--archive-dir` が無い、
   またはファイルが見つからない局も失敗になる。

「wdoor に棋譜が無い」と判定するのは、HTTP 404 で応答本文が `Not Found on wdoor` のときだけ
（サーバーの `kifu_import_api` が返す本文）。それ以外の 404 は API の場所が違う（`--api-base` が
`/api/v1` までを指していない、取り込み API が未デプロイ）ので、アーカイブへは切り替えず、
`--api-base` の確認を促すメッセージで失敗にする。

手元の `.csa`（`csa_client` の出力）は ID を集めるためだけに使い、本文は送らない。公開されるのは
両対局者の評価値・読み筋を含む wdoor 版の棋譜。

`--dry-run` では集めた対局 ID を 1 行 1 件で出す。

## jsonl: JSONL を CSA に変換して送る

```bash
# 1 局だけ
kifu_publish jsonl --input runs/sprt/pair-0-1__base-vs-test.jsonl --event SPRT-2026-10 --game-id 12
# ファイル内の終局した全局
kifu_publish jsonl --input ~/floodgate/records/jsonl/20261009_000001_RAMU_vs_Panda.jsonl --event local
# 送らずに変換結果だけ確認（標準出力へは 1 局分。複数局は --out-dir に書く）
kifu_publish jsonl --input game.jsonl --event test --game-id 1 --dry-run > game.csa
kifu_publish jsonl --input game.jsonl --event test --dry-run --out-dir ./csa-out
```

| 引数 | 説明 |
|---|---|
| `--input <FILE>` | `tournament` のペアファイルまたは `csa_client` の per-game JSONL（`.gz` 可） |
| `--event <LABEL>` | 一覧に出す大会名など。CSA の `$EVENT:` にも書く |
| `--game-id <N>` | この `game_id` の局だけを送る。省略時は `result` 行のある全局。`tournament` の `game_id` はペアファイルごとの連番 |
| `--out-dir <DIR>` | `--dry-run` 専用。標準出力の代わりに `<DIR>/<入力名>-<game_id>.csa` を書く。`--dry-run` で 2 局以上を変換するときは必須（指定がないと 2 局目でエラー終了する） |

`meta` / `move` / `result` 行から、対局ごとに次の内容の CSA を作り、
`POST {api-base}/admin/games/upload` に `source: "upload"` で送る。入力は 1 行ずつ読み、
`result` 行が現れた局から順に変換して送る。メモリに持つのは `result` 行がまだ来ていない局の
move 行で、`tournament` のように 1 局分をまとめて書くファイルなら 1 局分。

- 対局者名: `move` 行の `engine`（先手・後手それぞれ最初の手）。片方が 1 手も指していない局は、
  `csa_client` の JSONL なら相手側の `engine_cmd.path_*`（`remote:<名前>`）か `result.winner` から
  生の名前を取り、どちらも使えなければ `meta.engine_cmd` の 2 つのラベルのうち指した側でない方を使う
  （`csa_client` のラベルはファイル名用に `.` などを `_` に置き換えた名前）。
- 開始局面: 最初の手の `sfen_before`。平手以外（定跡の途中から始める SPRT、駒落ち、後手番開始）も
  そのまま開始局面として書く。
- 指し手・消費時間: `move_usi` を CSA の指し手に直し、`elapsed_ms` を秒に切り捨てて `T<秒>` に書く。
  終局用の擬似指し手（`resign` / `win` / `timeout` / `illegal` / `none`）と、時間切れで盤面に
  反映されなかった手は指し手行にしない。`tournament` の反則手は、元の手（`raw_move_usi`）を
  CSA に直せれば `'ILLEGAL_MOVE(<手>)` コメントとして残す。
- 評価値・読み筋: `eval.score_cp`（詰みは `score_mate` の符号で ±100000）を先手視点に直し、
  `eval.pv` を CSA の指し手列にして `'** <評価値> <読み筋>` に書く。読み筋の先頭は実際の指し手
  そのもの（`csa_client` が Floodgate へ送るコメントと同じ。wdoor の棋譜には、相手の応手から
  始める流儀のエンジンも混在する）。先頭が実際の指し手と違う読み筋は書かず、評価値だけ残す。
  途中に USI として読めない手・局面に適用できない手があれば、その手の前までを書く。
- 開始・終了時刻: `$START_TIME` は `meta.timestamp`（`tournament` では実行開始時刻）を JST に
  直した時刻。サーバーは CSA の時刻を JST として読む。`'$END_TIME:` は開始時刻 + 消費時間の合計
  （時間切れになった手など、指し手行にしない終局時の手の時間も含む）。
- 持ち時間: `meta.settings` の `btime` / `byoyomi` を `$TIME_LIMIT:HH:MM+SS`（持ち時間の時:分 +
  秒読みの秒）に、`binc` を `'Increment:<秒>` に書く。持ち時間の 1 分未満の端数は切り捨てる。
  すべて 0 なら省く。

出力の書式は [CSA の書式](#csa-の書式)のとおり。同じ入力・同じ `--event` からは常に同じ CSA が
でき、サーバーの棋譜 ID は本文から決まるので、送り直しても重複しない（`exists` になる）。
`--event` は本文の `$EVENT:` に入るため、**`--event` を変えて送り直すと別の棋譜 ID になり、
同じ対局が重複して登録される**。

### 終局理由の対応

JSONL の `result.reason` を CSA の終局行に直す。

| `reason` | 終局コマンド | `'summary:` の理由 |
|---|---|---|
| `resign` | `%TORYO` | `toryo` |
| `timeout`（tournament）/ `time_up`（csa_client） | `%TIME_UP` | `time up` |
| `illegal_move` | なし（反則手が分かれば `'ILLEGAL_MOVE(<手>)`） | `illegal move` |
| `win`（tournament、宣言の move 行あり）/ `win_declaration`（csa_client） | `%KACHI` | `kachi` |
| `sennichite_perpetual_check`（tournament）/ `oute_sennichite`（csa_client） | なし | `oute_sennichite` |
| `sennichite` | `%SENNICHITE` | `sennichite` |
| `jishogi` | `%JISHOGI` | `jishogi` |
| `max_moves` | `%MAX_MOVES` | `max_moves` |
| `abnormal` | `%CHUDAN` | `abnormal` |
| `adjudication_resign` / `adjudication_win`（tournament の評価値による裁定） | `%TORYO` | `toryo` |
| `win` / `lose`（csa_client、サーバーが理由行を送らなかった決着） | `%TORYO` | `toryo` |

変換できた対局には必ず `'summary:` 行があり、勝敗はその行が持つ。次の対局は変換せず、その局を
失敗として数える。

- 勝敗が確定していない局: 勝者がなく（`outcome` が `draw`）、理由が引き分け（`sennichite` /
  `jishogi` / `max_moves`）でもないもの。`csa_client` が自分の投了・入玉宣言の後にサーバーの
  結果を受け取れず切断した局（`reason` は `resign` / `win_declaration`）がこれに当たる。
- `error` の付いた不成立局、`adjudication_draw`・`no_bestmove`・`interrupted` など上の表にない理由。
- 指し手が 1 手もない局、指し手を局面に適用できない局。

## csa: CSA をそのまま送る

```bash
kifu_publish csa --input wcsc36-final.csa --event WCSC36
```

ファイルの内容を変更せず `POST {api-base}/admin/games/upload` に `source: "upload"` で送る。
サーバーは指し手を初期局面から再生できない棋譜を受け付けない。

入力は UTF-8 のみ。Shift_JIS などの棋譜は文字コード変換を行わず、エラーにする（対局者名が
文字化けしたまま登録されるのを避けるため）。事前に `iconv -f SHIFT_JIS -t UTF-8` などで UTF-8 に
変換してから渡す。

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

`--dry-run` のときは、標準出力には送る内容（対局 ID または CSA）だけを出し、件数の行と失敗の
記録は標準エラーに出す。

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
$TIME_LIMIT:00:05+00
'Increment:10
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

- 時刻は JST。`$TIME_LIMIT:HH:MM+SS` は持ち時間の時:分と秒読みの秒で、フィッシャーの加算は
  wdoor と同じ `'Increment:<秒>` コメントに書く（上の例は 5 分 + 1 手 10 秒加算）。
- `'** <評価値> <読み筋>` は直前の指し手のコメントで、評価値は先手視点。評価値のない手には書かない。
- `'summary:<理由>:<先手名> <win|lose|draw>:<後手名> <win|lose|draw>` が勝敗を持つ。勝敗が
  決まっている局には必ず書く。
- 反則負け（`illegal move`）と連続王手の千日手（`oute_sennichite`）は、wdoor の実物と同じく
  `%` コマンドを書かずに `'summary:` で終える。反則手は指し手行にせず、分かる場合は
  `'ILLEGAL_MOVE(<手>)` コメントにする。異常終了（`abnormal`）の終局コマンドは `%CHUDAN`
  （wdoor の実物は末尾が `%TORYO` のこともある。どちらも勝敗は `'summary:` で決まる）。
- `'summary:` の理由は、wdoor の実棋譜（2026 年 6〜7 月の約 2.4 万局）に現れる 9 種
  （`toryo` / `time up` / `sennichite` / `kachi` / `abnormal` / `illegal move` /
  `oute_sennichite` / `max_moves` / `oute_kaihimore`）の表記に合わせている。
  `oute_kaihimore`（王手放置）は CSA プロトコル上 `#ILLEGAL_MOVE` として届き区別できないので、
  このツールと `csa_client` は `illegal move` と書く。`jishogi`（`#JISHOGI` + `#DRAW` の持将棋
  引き分け）は wdoor にない独自のラベル。

`kifu_player` / `book_from_csa` などリポジトリ内の CSA の読み手は、`'summary:` 行があれば
終局コマンドより優先して勝敗を決める。

## テスト

```bash
cargo test -p tools --lib kifu_publish
```

対局 ID の収集、JSONL → CSA 変換（固定入力での出力一致、平手以外の開始局面、JST への変換、
サーバーの読み方での持ち時間の読み戻し、勝敗未確定の局の除外、壊れた読み筋、反則手のコメント化）、
HTTP 部分（ローカルのモックサーバーで成功 / 保存済み / 404 → アーカイブ / それ以外の 404 /
リダイレクト / 失敗）を検証する。変換した CSA の勝敗が既存の読み手で一致することは
`cargo test -p tools --lib csa_source` で検証する。
