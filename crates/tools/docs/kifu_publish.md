# kifu_publish

棋譜を rshogi の棋譜サーバー（`rshogi-csa-server-workers`）へ公開する CLI。公開した棋譜は
サーバーの一覧・検索・単局 API に載り、ビューアから閲覧できる。

| サブコマンド | 入力 | サーバーへの送り方 |
|---|---|---|
| `wdoor` | `csa_client` の棋譜ディレクトリ | 対局 ID だけを送り、サーバーが wdoor から棋譜を 1 回取得する |
| `jsonl` | `tournament` / `csa_client` の JSONL | wdoor 形式の CSA に変換して本文を送る |
| `csa` | CSA ファイル / ディレクトリ | サーバーが読める形に正規化して本文を送る（Shift_JIS 可） |
| `kif` | 電竜戦の KIF ファイル / ディレクトリ | CSA に変換して本文を送る |
| `event` | イベントの正式名称・公式リンクなど | イベントのメタデータを登録する（棋譜は送らない） |

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
| `--dry-run` | off | サーバーへ送らず、送る内容だけを標準出力に出す（件数などは標準エラー）。`RSHOGI_ADMIN_TOKEN` と `--api-base` は不要。`--out-dir` と併用するときはサブコマンドの後ろに書く |

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

## csa: CSA を正規化して送る

```bash
# 1 ファイル
kifu_publish csa --input wcsc36-final.csa --event WCSC36
# ディレクトリ（大会の公式アーカイブを展開したもの）
kifu_publish csa --input wcsc35_kifu/ --event WCSC35
kifu_publish csa --input wcsc31_kifu/ --event WCSC31 --fallback-start-time "2021/05/03 09:00:00"
# 送らずに確認（1 ファイルは標準出力、ディレクトリは --out-dir に書く）
kifu_publish csa --input wcsc35_kifu/ --event WCSC35 --dry-run --out-dir ./csa-out
```

| 引数 | 説明 |
|---|---|
| `--input <FILE\|DIR>` | CSA ファイル、またはディレクトリ。ディレクトリなら配下（サブディレクトリを含む）の `*.csa` をファイル名順に全部送る。`._` で始まるファイル（macOS が ZIP に入れるリソースフォーク）は無視する |
| `--event <LABEL>` | 一覧に出す大会名など |
| `--fallback-start-time <"YYYY/MM/DD HH:MM:SS">` | `$START_TIME` が無い棋譜に入れる仮の開始時刻（JST）。省略時、開始時刻が無い棋譜は送らない |
| `--out-dir <DIR>` | `--dry-run` 専用。正規化した CSA を、入力と同じ相対パスで `<DIR>` の下に書く |

文字コードは UTF-8 として読めなければ Shift_JIS（CP932）として読み、UTF-8 で送る。どちらでも
ないファイルは失敗になる。1 件の失敗では止めず、最後に件数を出す。

CP932 として読むのは割り当ての確かな文字だけで、次のものを含むファイルは読み取り失敗にする
（文字化けした対局者名を登録しないための意図的な制限）。

- 1 バイトの `0x80` / `0xA0` / `0xFD`〜`0xFF`（CP932 に割り当てが無い）。
- 外字（第 1 バイト `0xF0`〜`0xF9`。Unicode の私用領域 U+E000〜U+F8FF に写るだけで、作成者の
  環境の外字フォントが無いと何の文字か分からない）。
- 対応表に無い 2 バイトの組。

対応表（`crates/tools/src/kifu_publish/cp932_table.bin`）は Python の `cp932` codec から生成した
もので、テストが SHA-256 を固定している。再生成のコマンドは `cp932.rs` のコメントにある。

送る前に、サーバーが読める形へ次の正規化をする（WCSC31〜35・WCSO1 の公式アーカイブで見つかった
書式のゆれが対象。サーバーが元々読める棋譜は内容を変えない）。改行は LF にそろえ、行末の空白は
落とす。

サーバーの棋譜 ID は本文の SHA-256 から決まる。`$START_TIME` がある棋譜の正規化結果は
そのファイルの内容だけで決まるので、送り直しても重複しない（正規化済みの CSA をもう一度通しても
変わらない）。**`$START_TIME` を補完した棋譜の本文（= 棋譜 ID）は、投入単位に依存する**（下記）。

| 入力 | 出力 |
|---|---|
| 先頭の `'CSA encoding=SHIFT_JIS` | 落とす（UTF-8 で送るため） |
| 指し手・`%` 行の後ろに単独である `T<秒>`（WCSC31 / 32・WCSO1） | 直前の指し手・`%` 行に `,T<秒>` として連結（間のコメント行・空行は飛び越える。`+7776FU,'memo` のようなインラインコメントつきの行は `+7776FU,T3,'memo` にする） |
| `$TIME:900+0+5`（CSA V3.0 の総秒 + 秒読み + 加算。WCSC35） | 元の行の次に `$TIME_LIMIT:00:15+00` と `'Increment:5` を足す |
| `$TIME_LIMIT:15分＋手番×5秒`（WCSC33）/ `'$TIME_LIMIT:15分＋手番×5秒`（WCSC34） | 元の行はコメントにして残し、`$TIME_LIMIT:00:15+00` と `'Increment:5` を足す |
| `$START_TIME` が無い | 局面（`PI` / `P1`）の直前に `$START_TIME:` とその由来のコメント行を足す（下記） |

- 持ち時間は、上の書式以外（総秒が 60 で割り切れない `$TIME:` など）には触らない。持ち時間の
  記載が無い棋譜に推測で足すこともしない（その対局は持ち時間なしで登録される）。
- サーバーは `$START_TIME` の無い棋譜を受け付けない。開始時刻が無い棋譜には、同じディレクトリ・
  同じ回戦（ファイル名の「大会_回戦」部分。`WCSC31_L8_AUA_ABZ.csa` なら `WCSC31_L8`）の他の棋譜の
  うち最も早い `$START_TIME` を入れる。それも無ければ `--fallback-start-time` の値を入れる。
  どちらも無い棋譜は送らず、`skipped <ファイル>: 開始時刻なし` として `excluded` に数える。
  足した行の次には、由来を示すコメント行（`'開始時刻は記録が無いため…`）が入る。
- **補完した棋譜の棋譜 ID は投入単位（ディレクトリの中身）に依存する。** 同じ回戦から取る値は、
  一緒に処理したファイルの集合で変わる（1 ファイルだけを指定すると他の棋譜を参照できない。
  同じ回戦の棋譜が増減すると最も早い開始時刻が変わりうる）。`--fallback-start-time` の値を
  変えても変わる。補完値が変わると本文が変わり、別の棋譜 ID で重複して登録される。
  **再投入は必ず同じディレクトリ単位・同じ `--fallback-start-time` で行う。**
  補完した棋譜は、ファイルごとに採用した時刻と方法を標準エラーに出す
  （`start-time <入力ディレクトリからの相対パス>: <時刻> (同じ回戦の最も早い開始時刻 | --fallback-start-time)`、
  最後に `filled_start_time=<件数>`）。
- 局面より後ろの単独の `T<秒>` 行に連結先が無い（直前の指し手・`%` 行に既に時間が付いている、
  指し手がまだ無い）棋譜は、消費時間を落とさずに変換失敗
  （`failed[消費時間の行を指し手に連結できない]`）にする。
- 以前の版の `csa` サブコマンドはファイルの内容を変えずに送っていた。行末の空白・CRLF・BOM の
  ある UTF-8 の CSA は、この版では LF・行末空白なしに正規化されて本文が変わるので、以前の版で
  送った同じファイルとは別の棋譜 ID になる。
- `V3.0`、`$MAX_MOVES` / `$JISHOGI` / `$SITE`、盤面の `P1`〜`P9`、`'** <評価値> <読み筋>` など、
  それ以外の行はそのまま送る。

サーバーは指し手を初期局面から再生できない棋譜を受け付けない（HTTP 422、その局は `failed`）。
最後の手が反則手の棋譜は、末尾が `%ILLEGAL_MOVE` のときだけ受け付ける。

## kif: 電竜戦の KIF を CSA に変換して送る

```bash
kifu_publish kif --input kifu_dr6production/ --event 電竜戦6
kifu_publish kif --input game.kif --event 電竜戦6 --dry-run > game.csa
kifu_publish kif --input kifu_dr6production/ --event 電竜戦6 --dry-run --out-dir ./csa-out
```

| 引数 | 説明 |
|---|---|
| `--input <FILE\|DIR>` | KIF ファイル、またはディレクトリ（配下の `*.kif` をファイル名順に全部。`._` で始まるファイルは無視） |
| `--event <LABEL>` | 一覧に出す大会名など |
| `--out-dir <DIR>` | `--dry-run` 専用。変換した CSA を、入力と同じ相対パス（拡張子は `.csa`）で `<DIR>` の下に書く |

電竜戦の公式 KIF（第 2〜6 回の本戦アーカイブで確認した形式。文字コードは `csa` と同じく
UTF-8 でなければ Shift_JIS として読む）を CSA に変換し、`csa` と同じく
`POST {api-base}/admin/games/upload` に `source: "upload"` で送る。移動元が括弧で書かれている
（`７六歩(77)`）ので盤面を追わずに変換する。合法性はサーバーが取り込み時に確かめる。

```text
V2.2
N+<先手>
N-<後手>
$EVENT:<棋戦>
$START_TIME:<開始日時>
$TIME_LIMIT:00:10+00
'Increment:2
'先手持ち時間420秒削減
'指定局面名：先手3分
PI
+
+5958OU,T3
'** 122 -5251OU +4859OU -3334FU
...
%TORYO
'まで84手で後手の勝ち
```

- ヘッダ: `先手：` / `後手：`（末尾の空白・タブは落とす）、`棋戦：`、`開始日時：`。
  `*時間設定：10分-1手2秒加算` は `$TIME_LIMIT:00:10+00` と `'Increment:2` にする（この書式以外は
  持ち時間を書かない）。`*先手持ち時間…秒削減` / `*後手持ち時間…秒削減` と `*指定局面名：…` は
  コメントとして残す。それ以外の `*` 行（順位・勝敗・戦型タグなど）は落とす。
- 指し手: `同` は直前の移動先、`打` は移動元 `00`。CSA には移動後の駒種を書く（`３三角成(88)` →
  `8833UM`、`２二成香(32)` → `3222NY`）。消費時間 `( m:ss/…)` は `,T<秒>`。
- 評価値: `**対局 評価値 <数値> 読み筋 <手…>` は、直前の指し手の次の行に
  `'** <数値> <CSA の読み筋>` として書く（数値は KIF に書かれた値のまま）。読み筋は
  変換できない手が出たところで切る。数値でない評価値はコメントごと落とす。
- 元の `まで…` 行は末尾にコメントとして残す。

### 終局表記の対応

| KIF の最後の手 | 終局コマンド |
|---|---|
| `投了` | `%TORYO` |
| `千日手` | `%SENNICHITE` |
| `切れ負け` | `%TIME_UP` |
| `入玉宣言` / `入玉勝ち` | `%KACHI`（宣言した側 = その手番の勝ち） |
| `持将棋` | `%JISHOGI`（`*終局理由：最大手数` なら `%MAX_MOVES`） |
| `反則負け`（非合法手・王手千日手） | `まで` 行の勝者の反対側の `%+ILLEGAL_ACTION` / `%-ILLEGAL_ACTION` |
| 終局の手も `まで` 行も無い（`*終局理由：接続切れ` で指し手だけ、など） | `%CHUDAN` |

`中断` / `詰み` など、表に無い終局表記の棋譜は変換せず `failed[未対応の終局表記]` になる。
同じ宣言の行が 2 行続く形（`入玉宣言` / `入玉勝ち`。電竜戦の棋譜はこの形）は 1 回の宣言として読む。

次の棋譜は送らない。

- `excluded`（対象外）: 指し手が 1 つも無い（`指し手なし`）、`手合割` が平手でない（`平手以外`）、
  `開始日時` が無い・読めない（`開始時刻なし`）。
- `failed`（変換失敗）:
  - 変換できない指し手、手数が連続していない、上の表に無い終局表記。
  - 終局の手の後に指し手や別の終局表記、変化手順などの行が続く（`終局の手の後に指し手が続く`。
    どこまでが対局か決められないので、最初の終局で切って送ることはしない。`まで` 行・空行・
    `*` で始まるコメント行は続いてよい）。
  - `反則負け` なのに `まで` 行から勝者が読めない。
  - `まで…先手の勝ち / 後手の勝ち / 引分け` と変換結果（サーバーが終局コマンドと手番から決める
    勝敗）が食い違う。終局の手が無いのに `まで` 行がある棋譜もこれに含める（`%CHUDAN` で送ると
    記録された結果が消えるため）。
  - `まで N手で…` の N が指し手の数と合わない（`手数が まで 行と合わない`）。N は終局の手を
    除いた指し手の数。入玉の宣言だけは宣言の行も 1 手に数える（317 手指して 318 手目に宣言なら
    `まで318手`）。

## event: イベントのメタデータを登録する

```bash
kifu_publish event --event WCSC35 \
    --title "第35回世界コンピュータ将棋選手権" \
    --organizer "コンピュータ将棋協会" \
    --official-url https://example.org/wcsc35/ \
    --kifu-source-url https://example.org/wcsc35/kifu/ \
    --terms-note "棋譜の利用条件の要約"
```

| 引数 | 説明 |
|---|---|
| `--event <LABEL>` | 対象のイベント。棋譜を送るときの `--event` と同じ文字列（前後の空白は除かれる） |
| `--title <TEXT>` | 正式名称（必須、200 文字まで） |
| `--organizer <TEXT>` | 主催（200 文字まで） |
| `--official-url <URL>` | 大会の公式ページ。`https://` か `http://` で始まる URL（500 文字まで）。ユーザー情報つき（`https://user@host/`）は不可 |
| `--kifu-source-url <URL>` | 棋譜の配布元ページ。同上 |
| `--terms-note <TEXT>` | 利用条件の要約（1 行、1000 文字まで） |

`PUT {api-base}/admin/events/{event}` を送る。`GET {api-base}/games/events` の該当イベントに
`meta` として付き、ビューアが大会名や公式リンクの表示に使う。棋譜を 1 局も送っていない
イベントにも登録できる。同じイベントへの再実行は全体の置き換えで、省略した任意項目は未登録に戻る。
入力の検証（文字数、制御文字、URL の形）はサーバーが行い、不正なら HTTP 400 で失敗する。

本文に知らない項目があるとサーバーは 400 を返す（全体の置き換えなので、項目名の誤りを黙って
捨てない）。

`--dry-run` では送る JSON を標準出力に出す。成功時は `updated <event>` を出す（件数の行は出さない）。

## 出力と終了コード

1 局ごとに結果を出し、最後に件数を出す。失敗した局は標準エラーに記録して次の局へ進む。

```text
created wdoor-20261009000001-0123456789 wdoor+floodgate-300-10F+RAMU+Panda+20261009000001
exists  wdoor-20261009003000-abcdef0123 wdoor+floodgate-300-10F+RAMU+Other+20261009003000
failed  wdoor+floodgate-300-10F+Old+RAMU_TF+20260706123000: wdoor に棋譜がなく、... にも見つかりません
created=1 skipped=1 excluded=0 failed=1
```

| 表示 | 意味 |
|---|---|
| `created` | 新しく保存された |
| `exists`（件数は `skipped`） | サーバーに保存済みで何もしなかった（応答の `created` が `false`） |
| `skipped <ファイル>: <理由>`（件数は `excluded`） | 送る前に対象外にした（`csa` / `kif` の開始時刻なし・指し手なし・平手以外） |
| `failed` | 送信・変換に失敗した（HTTP ステータスと応答本文の先頭を表示） |

`--dry-run` のときは、標準出力には送る内容（対局 ID または CSA）だけを出し、件数の行と失敗の
記録は標準エラーに出す。

`csa` / `kif` は、件数の行の前に、読んだ文字コードごとのファイル数、変換できた局数、
送らなかった理由ごとの件数を標準エラーに出す。
ディレクトリを `--dry-run` で `--out-dir` なしに通すと、CSA は出さずに 1 局ごとの
`ok <ファイル>` とこの集計だけを出す。

```text
decoded[utf-8]=78
decoded[cp932]=220
converted=477
excluded[指し手なし]=1
failed[手数が連続していない]=1
```

`--dry-run --out-dir <DIR>` で書き出した CSA は、サーバーの取り込みと同じ検証（全手の合法性、
結果の導出。KIF から変換したものは `まで` 行の勝敗との一致も）にまとめて通せる。大会の
アーカイブを送る前の確認に使う。

```bash
KIFU_VERIFY_DIR=<DIR> cargo test -p rshogi-csa-server-workers \
    --test kifu_archive_import -- --ignored --nocapture
```

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

文字コードの判定と Shift_JIS の読み取り、CSA の正規化と KIF → CSA 変換（公式棋譜の短いサンプル
`tests/fixtures/kifu_publish/` からの出力が `tests/golden/kifu_publish/` と一致すること、2 回通しても
変わらないこと、終局表記・評価値コメント・対象外の棋譜）、
対局 ID の収集、JSONL → CSA 変換（固定入力での出力一致、平手以外の開始局面、JST への変換、
サーバーの読み方での持ち時間の読み戻し、勝敗未確定の局の除外、壊れた読み筋、反則手のコメント化）、
HTTP 部分（ローカルのモックサーバーで成功 / 保存済み / 404 → アーカイブ / それ以外の 404 /
リダイレクト / 失敗、イベントメタの `PUT`）を検証する。golden の CSA がサーバーの取り込みを通ることは
`cargo test -p rshogi-csa-server-workers --test kifu_archive_import` で検証する。変換した CSA の勝敗が既存の読み手で一致することは
`cargo test -p tools --lib csa_source` で検証する。
