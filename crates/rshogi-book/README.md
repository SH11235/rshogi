# rshogi-book

`rshogi-book` は YANEURAOU-DB2016 テキスト `.db` 形式と YaneuraOu のバイナリ定跡 `.ybb` の
定跡を読み込み、root 局面で定跡手を 1 手 probe する crate です。USI 統合では `rshogi-usi` の Book 系オプションが
`BookOptions` に反映されます。

## 定跡 probe オプション

| USI オプション | 既定 | 説明 |
| --- | --- | --- |
| `USI_OwnBook` | `true` | 定跡使用の総合スイッチ。`false` なら probe しない。 |
| `BookFile` | `no_book` | 定跡ファイル名 (`.db` か `.ybb`)。`no_book` または空なら定跡をロードしない。`.db` を指定してそのファイルが無く、同名の `.ybb` があればそちらを読む。[下記](#yaneuraou-のバイナリ定跡-ybb)参照。 |
| `BookExploreFile` | `<empty>`（空文字列・無効） | 指定局面の定跡手を等確率で選ぶ UTF-8 ファイル。 |
| `BookDir` | `book` | 相対 `BookFile` を解決するディレクトリ。 |
| `BookMoves` | `16` | この手数まで定跡を使う。 |
| `BookEvalDiff` | `30` | 候補中の最大 value からこの差分以内の手だけ残す。 |
| `BookEvalBlackLimit` | `0` | 先手番で採用する value 下限。 |
| `BookEvalWhiteLimit` | `-140` | 後手番で採用する value 下限。 |
| `BookDepthLimit` | `0` | 筆頭手の depth 下限。`0` は無効。 |
| `NarrowBook` | `false` | count 情報がある場合、出現率 10% 未満の手を除外する。 |
| `BookSelectValue` | `false` | 評価値フィルタ後の生存候補から value 最大手を決定的に選ぶ。同値は count 大、さらに同値なら USI 昇順。`true` のとき `NarrowBook` / `ConsiderBookMoveCount` / 等確率抽選より優先する。 |
| `ConsiderBookMoveCount` | `false` | `true` なら count 比例抽選する。全 count が 0 の場合は等確率。 |
| `IgnoreBookPly` | `false` | SFEN の手数を無視して局面を引く。 |
| `FlippedBook` | `true` | miss 時に先後反転局面で再検索する。 |
| `BookOnTheFly` | `false` | `true` なら定跡を丸読みせず、probe のたびにファイルから引く。大きい定跡向け。[下記](#大きい定跡を丸読みせずに使う-bookonthefly)参照。 |

選択順序は、合法性検証、`BookDepthLimit`、`BookEvalDiff` / value 下限フィルタの後に
`BookSelectValue` を評価します。`BookSelectValue=false` の場合は従来どおり
`NarrowBook` を適用し、その後 `ConsiderBookMoveCount` または等確率抽選で選びます。

## 大きい定跡を丸読みせずに使う (BookOnTheFly)

既定 (`BookOnTheFly=false`) では `isready` で定跡ファイル全体をメモリに読み込みます。メモリと
`isready` の時間が定跡の大きさに比例して増えるので、数百万局面を超える定跡では
`setoption name BookOnTheFly value true` を使います。probe の結果 (候補手・value・depth・
採択数と、各フィルタ・`IgnoreBookPly`・`FlippedBook`・`BookExploreFile` を通した後の選択) は
丸読みと同じです。

- **ファイルの並び**: `sfen` 行が SFEN の文字列 (バイト) 順に並んでいる必要があります。
  rshogi の定跡ツールの出力と YaneuraOu の定跡書き出し (`.db`) はこの順です。手数の違う同じ
  局面が隣り合っていれば、手数込みで並べても手数を除いて並べてもかまいません。`isready` で
  ファイルの 1,024 か所を抜き取って並びを確かめ、逆順を見つけたら
  `info string Error opening book file ...: book file is not sorted by SFEN ...` を出して
  定跡なしで続けます。抜き取りなので局所的な乱れは見逃し得ます (その局面は引けません)。
- **持つもの**: 開いたファイルハンドルだけで、索引は作りません。エンジンのメモリは定跡の
  大きさによらず増えません。
- **`isready`**: ファイルを開いて並びを抜き取りで確かめるだけで、定跡の大きさによらず短く
  終わります。
- **probe**: ファイル上の二分探索で局面を引きます。1 回の検索で読むのは、ファイルの大きさの
  対数に比例する回数の小さな読み出し (1 回 8 KiB 程度) と、手数を除いて同じ局面の行
  (手数違いの重複があればその全部) です。OS のファイルキャッシュに載っていればミリ秒未満で、
  載っていないときは記憶装置のランダム読み出しの速さで決まります。ponder の補完や
  `FlippedBook` の再検索で 1 回の probe に数回の検索をします。
- **読み出しエラー**: probe 中の読み出しエラーや UTF-8 でない行は、その局面を定跡なしとして
  `info string Error reading book file: ...` を出します。ponder を補うために子局面を引いて
  いるときのエラーは何も出さず、予想手なしで bestmove を返します。丸読みはこの場合 `isready`
  で読み込み自体が失敗します。
- 上の並び・抜き取り・読み出し量の説明はテキスト `.db` のものです。`.ybb` は[下記](#yaneuraou-のバイナリ定跡-ybb)。

### 定跡ファイルの差し替え

`BookOnTheFly=true` では `isready` のたびにファイルを開き直します。対局の合間に新しい定跡を
置いて `isready` を送れば、次の対局から新しいファイルを引きます。

- 差し替えは別名で書き出してから `mv` で置き換える (rename) か、別のファイル名にして
  `BookFile` を変えます。開いているファイルをその場で上書きすると、次の `isready` までの
  probe が書きかけの内容を読むことがあります。
- Windows では開いているファイルの置き換えが失敗することがあります。その場合は別のファイル名に
  して `BookFile` を変えてください。

丸読み (`BookOnTheFly=false`) は、`BookFile` (解決後のパス) と `IgnoreBookPly` が前回と同じなら
`isready` で読み直しません。同じパスのファイルを差し替えたときは、エンジンを起動し直すか
`BookFile` を変えてください。

### 丸読みとの一致の確かめ方

実際の定跡ファイルで、全局面 (手数違いと先後反転を含む) の引いた結果と probe の選択が丸読みと
一致することを次のテストで確かめられます。

```bash
RSHOGI_BOOK_EQUIV_PATH=/path/to/book.db cargo test --release -p rshogi-book -- --ignored
```

## YaneuraOu のバイナリ定跡 (.ybb)

`BookFile` に YaneuraOu の `.ybb` (`YANE-BINBOOK-V1`) を指定すると、テキスト `.db` と同じ
オプションで使えます。形式はファイル先頭の magic で見分けます。`BookFile` に `.db` を指定して
そのファイルが無く、拡張子を `.ybb` にしたファイルがあればそちらを読み、
`info string book file fallback: <.db> -> <.ybb>` を出します (YaneuraOu と同じ)。

- **引き方**: 局面から YaneuraOu と同じビット列の PackedSfen を作り、PackedSfen の順に並んだ
  index を二分探索します。`IgnoreBookPly=false` では手数も一致したときだけ当たります (`.ybb` は
  同じ盤面を 1 つしか持てないので、手数違いの重複はありません)。`FlippedBook` は先後反転した
  局面で引き直し、手を反転します。玉が欠けた局面など PackedSfen にできない局面は引けません。
- **持たない情報**: ponder と採択回数 (count) を持たず、手ごとの depth は持つ版と持たない版
  (0 として扱う) があります。ponder は子局面を引いた筆頭手で補い、`ConsiderBookMoveCount` は
  等確率、`NarrowBook` は何もしません。同じ局面集合の `.db` (ponder なし・count が揃ったもの) から
  変換した `.ybb` は、probe の結果が `.db` と一致します。
- **丸読み (`BookOnTheFly=false`)**: ファイル全体をそのまま読み込みます。メモリはファイルの大きさと
  同じです。`isready` の時間はファイルを読む時間です。
- **`BookOnTheFly=true`**: header (32 byte) だけを読み、index も手もファイル上で引きます。並びの
  抜き取り検査はしません (YaneuraOu の書き出しは PackedSfen 順)。1 回の検索で読むのは index の
  44 byte を局面数の対数回と、その局面の手だけです。
- **壊れたファイル**: magic・未知の flags・局面数に対して短すぎるファイルは開くときに拒否します。
  手の領域が途中で切れている局面は probe の読み出しエラーとして扱います。

実際の `.ybb` と、同じ局面集合の `.db` (手数を除いた SFEN 順に並んだもの) で引いた結果と probe の
選択が一致することを、`.db` から等間隔に抜いた局面で確かめられます (`.db` を省くと平手を引くだけ)。

```bash
RSHOGI_YBB_PATH=/path/to/book.ybb RSHOGI_YBB_DB_PATH=/path/to/book.db \
  cargo test --release -p rshogi-book -- --ignored real_ybb
```

## 局面を指定した定跡手の乱択

`setoption name BookExploreFile value book/explore.txt` で指定し、`isready` で読み込みます。
空文字列または `setoption name BookExploreFile value <empty>` で無効に戻せます。
相対パスは実行時の作業ディレクトリ基準です（`BookDir` は使いません）。パスが変わると
次の `isready` で再読み込みします。読み込み成功後に同じパスを再読み込みするには、一度空に戻して再指定します。

```text
# 平手で二つの定跡手を試す（手数欄は省略可）
sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1 7g7f 2g2f
```

空行と、空白を除いた行頭が `#` の行は無視します。各行は
`sfen <盤面> <手番> <持駒> [<手数>] <USI手> [<USI手> ...]` です。
局面キーは持駒順などを正規化し、手数を無視します。同じ局面の行は後勝ち、同じ行の重複手は一つにまとめます。
不正行は行番号付きの `info string` 警告を出して無視し、読み込み後に登録局面数を通知します。
ファイルを読めない場合（UTF-8 不正を含む）は警告して未ロードのままとし、
同じパスでも次の `isready` で読み込みを再試行します。

`USI_OwnBook`、`BookMoves` と定跡ヒットの条件を満たした局面だけが対象です。
直接一致がなく `FlippedBook=true` なら反転局面も検索し、指定手を反転して使います。
指定手のうち合法かつ定跡に存在する手を記載順に候補とし、既存の `BookRng` で等確率に選びます。
この選択は評価値差・評価値下限・深さ・`NarrowBook`・`BookSelectValue`・
`ConsiderBookMoveCount` より優先し、ponder は選ばれた定跡手から通常どおり解決します。
非合法手や定跡にない手は除外し、局面・手の組ごとに読み込み後一度だけ警告します。
候補が残らなければ通常の選択へ戻り、リストにない局面や空の設定では従来の動作を保ちます。
選択時は `info string book explore: 2g2f from [7g7f, 2g2f]` の形式で記録します。
