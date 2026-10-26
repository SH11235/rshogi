# rshogi-book

`rshogi-book` は YANEURAOU-DB2016 テキスト `.db` 形式と YaneuraOu のバイナリ定跡 `.ybb` の
定跡を読み込み、root 局面で定跡手を 1 手 probe する crate です。USI 統合では `rshogi-usi` の Book 系オプションが
`BookOptions` に反映されます。

## 定跡 probe オプション

| USI オプション | 既定 | 説明 |
| --- | --- | --- |
| `USI_OwnBook` | `true` | 定跡使用の総合スイッチ。`false` なら probe しない。 |
| `BookFile` | `no_book` | 定跡ファイル名 (`.db` か `.ybb`)。`no_book` または空なら定跡をロードしない。`.db` を指定してそのファイルが無く、同名の `.ybb` があればそちらを読む。[下記](#yaneuraou-のバイナリ定跡-ybb)参照。同じディレクトリに `<名前>-000.db` などの優先定跡があれば先に引く。[下記](#優先定跡)参照。 |
| `BookExploreFile` | `<empty>`（空文字列・無効） | 指定局面の定跡手を等確率で選ぶ UTF-8 ファイル。 |
| `BookDir` | `book` | 相対 `BookFile` を解決するディレクトリ。 |
| `BookMoves` | `16` | この手数まで定跡を使う。 |
| `BookEvalDiff` | `30` | 候補中の最大 value からこの差分以内の手だけ残す。 |
| `BookEvalBlackLimit` | `0` | 先手番で採用する value 下限。 |
| `BookEvalWhiteLimit` | `-140` | 後手番で採用する value 下限。 |
| `BookDepthLimit` | `0` | 筆頭手の depth 下限。`0` は無効。 |
| `BookEvalBlackDiff` / `BookEvalWhiteDiff` | `-1` | root 局面が先手番 / 後手番のときの `BookEvalDiff`。`-1` なら `BookEvalDiff` を使う。範囲は `-1`〜`99999`。[下記](#先後別の評価値差と-depth-下限)参照。 |
| `BookDepthBlackLimit` / `BookDepthWhiteLimit` | `-1` | root 局面が先手番 / 後手番のときの `BookDepthLimit`。`-1` なら `BookDepthLimit` を使う。`0` は無効。範囲は `-1`〜`99999`。 |
| `NarrowBook` | `false` | count 情報がある場合、出現率 10% 未満の手を除外する。 |
| `BookSelectValue` | `false` | 評価値フィルタ後の生存候補から value 最大手を決定的に選ぶ。同値は count 大、さらに同値なら USI 昇順。`true` のとき `NarrowBook` / `ConsiderBookMoveCount` / 等確率抽選より優先する。 |
| `ConsiderBookMoveCount` | `false` | `true` なら count 比例抽選する。全 count が 0 の場合は等確率。 |
| `IgnoreBookPly` | `false` | SFEN の手数を無視して局面を引く。 |
| `FlippedBook` | `true` | miss 時に先後反転局面で再検索する。 |
| `BookOnTheFly` | `false` | `true` なら定跡を丸読みせず、probe のたびにファイルから引く。大きい定跡向け。[下記](#大きい定跡を丸読みせずに使う-bookonthefly)参照。 |

選択順序は、合法性検証、`BookDepthLimit`、`BookEvalDiff` / value 下限フィルタの後に
`BookSelectValue` を評価します。`BookSelectValue=false` の場合は従来どおり
`NarrowBook` を適用し、その後 `ConsiderBookMoveCount` または等確率抽選で選びます。

## 先後別の評価値差と depth 下限

`BookEvalBlackDiff` / `BookEvalWhiteDiff` / `BookDepthBlackLimit` / `BookDepthWhiteLimit` で、
`BookEvalDiff` / `BookDepthLimit` を先手番と後手番で別の値にできます。名前と、`0` 以上の値の意味は
YaneuraOu の `BOOK_OPTIONS V2` の同名オプションと同じです。ただし評価値差の基準は、rshogi が候補中の
最大 value、YaneuraOu が count 順 (同 count なら value 順) の筆頭手の value なので、count の付いた定跡で
count 最大の手が最大 value でないときは残る手が変わります。たとえば後手番で (value 100, count 10) と
(value 120, count 5) の 2 手、`BookEvalWhiteDiff=0` なら、rshogi は value 120 の手だけを、YaneuraOu は
両方を残します。count を持たない定跡 (`.ybb` やペタショック定跡) では一致します。この差は共通の
`BookEvalDiff` にもあります。

- probe のたびに root 局面の手番側の値を見て、`0` 以上ならそれを、`-1` なら共通の値 (`BookEvalDiff` /
  `BookDepthLimit`) をその時点の値で使います。評価値差と depth 下限は別々に決まります。
- `FlippedBook` で先後反転した局面に当たったときも、使うのは元の root 局面の手番の値です。
- `setoption` の順序によらず結果は同じです。共通の値を後から変えても個別の値は残り、個別の値に
  `-1` を送ると共通の値を使う状態に戻ります。範囲外や数でない値は無視し、標準エラー出力に警告 (`info string Warning: ...`) を出します。
- 定跡手を絞ったときの info string には、使った値のオプション名 (共通なら `BookEvalDiff`、個別なら
  `BookEvalBlackDiff` 等) を出します。

例: `BookEvalDiff=30`、`BookEvalBlackDiff=10`、`BookEvalWhiteDiff=-1` なら、評価値差は先手番 10・後手番 30。
ここで `BookEvalDiff=50` にすると 10 / 50、さらに `BookEvalBlackDiff=-1` にすると 50 / 50 です。

YaneuraOu の `BOOK_OPTIONS V2` の既定値に相当する設定にするには、次を明示的に送ります
(`BookFile` は使う定跡に合わせる)。

```text
setoption name BookFile value user_book1.db
setoption name BookMoves value 200
setoption name BookEvalBlackDiff value 0
setoption name BookEvalWhiteDiff value 0
setoption name BookDepthBlackLimit value 0
setoption name BookDepthWhiteLimit value 5
setoption name NarrowBook value false
setoption name ConsiderBookMoveCount value false
setoption name IgnoreBookPly value true
setoption name BookOnTheFly value true
```

## 大きい定跡を丸読みせずに使う (BookOnTheFly)

既定 (`BookOnTheFly=false`) では `isready` で定跡ファイル全体をメモリに読み込みます。メモリと
`isready` の時間が定跡の大きさに比例して増えるので、数百万局面を超える定跡では
`setoption name BookOnTheFly value true` を使います。probe の結果 (候補手・value・depth・
採択数と、各フィルタ・`IgnoreBookPly`・`FlippedBook`・`BookExploreFile` を通した後の選択) は
手の無い局面 (下記) を除いて丸読みと同じです。

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
- **手の無い局面**: 手の行が無い `sfen` 行に当たったら定跡なしとし、`FlippedBook` でも反転局面を
  引き直しません (YaneuraOu の on the fly と同じ)。丸読みはこの局面を登録しないので反転局面を
  引き直し、選ぶ手が丸読みと違い得る唯一の場合です。
- 上の並び・抜き取り・読み出し量の説明はテキスト `.db` のものです。`.ybb` は[下記](#yaneuraou-のバイナリ定跡-ybb)。

### 定跡ファイルの差し替え

`BookOnTheFly=true` では `isready` のたびにファイルを開き直します。対局の合間に新しい定跡を
置いて `isready` を送れば、次の対局から新しいファイルを引きます。

- 差し替えは別名で書き出してから `mv` で置き換える (rename) か、別のファイル名にして
  `BookFile` を変えます。開いているファイルをその場で上書きすると、次の `isready` までの
  probe が書きかけの内容を読むことがあります。
- Windows では開いているファイルの置き換えが失敗することがあります。その場合は別のファイル名に
  して `BookFile` を変えてください。

丸読み (`BookOnTheFly=false`) は、読むファイルのパス (優先定跡を含む解決後のもの) と
`IgnoreBookPly` が前回と同じなら `isready` で読み直しません。同じパスのファイルを差し替えたときは、エンジンを起動し直すか
`BookFile` を変えてください。

### 丸読みとの一致の確かめ方

実際の定跡ファイルで、全局面 (手数違いと先後反転を含む) の引いた結果と probe の選択が丸読みと
一致することを次のテストで確かめられます (手の無い局面は除きます)。

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
- **持たない情報**: ponder と採択回数 (count) を持たず、手ごとの depth は持つ版 (flags の bit0)
  と持たない版 (0 として扱う) があります。ponder は子局面を引いた筆頭手で補い、
  `ConsiderBookMoveCount` は等確率、`NarrowBook` は何もしません。
- **`.db` と probe の結果が一致する条件**: 同じ局面集合の `.db` から変換した `.ybb` で、`.db` が
  ponder を持たず、count が局面内で揃っていて、`.ybb` が depth を持つ (flags の bit0) か
  `BookDepthLimit=0` であり、count を使うオプションを使わない (`NarrowBook=false` かつ
  `ConsiderBookMoveCount=false`) こと。count を使うと、`.db` は count で手を絞り重み付きで抽選する
  のに対し、`.ybb` は絞らず等確率なので選ぶ手が変わり得ます。
- **手の無い局面**: 当たった局面の手が 0 個なら定跡なしとし、`FlippedBook` でも反転局面を
  引き直しません (YaneuraOu と同じ)。手数が違って当たらなかったときは引き直します。
- **丸読み (`BookOnTheFly=false`)**: ファイル全体をそのまま読み込みます。メモリはファイルの大きさと
  同じで、`BookFile` などを変えて読み直すときは新旧が揃うまで一時的に 2 倍になります。大きい
  `.ybb` では `BookOnTheFly=true` を勧めます。`isready` の時間はファイルを読む時間です。
- **`BookOnTheFly=true`**: header (32 byte) だけを読み、index も手もファイル上で引きます。並びの
  抜き取り検査はしません (YaneuraOu の書き出しは PackedSfen 順)。1 回の検索で読むのは index の
  44 byte を局面数の対数回と、その局面の手だけです。
- **壊れたファイル**: 未知の flags・局面数に対して短すぎるファイル、拡張子が `.ybb` で magic が
  違うファイルは開くときに拒否します (header を確かめてから本体を読みます)。拡張子が `.ybb` でなく
  magic も違うファイルはテキスト `.db` として読みます。
  手の領域が途中で切れている局面は probe の読み出しエラーとして扱います。

実際の `.ybb` と、同じ局面集合の `.db` (手数を除いた SFEN 順に並んだもの) で引いた結果と probe の
選択が一致することを、`.db` から等間隔に抜いた局面で確かめられます (`.db` を省くと平手を引くだけ)。

```bash
RSHOGI_YBB_PATH=/path/to/book.ybb RSHOGI_YBB_DB_PATH=/path/to/book.db \
  cargo test --release -p rshogi-book -- --ignored real_ybb
```

## 優先定跡

YaneuraOu の優先定跡と同じ規則で、`BookFile` の前に別の定跡ファイルを引きます (probe 中の
読み出しエラーの扱いだけは意図して違います。下記)。
`BookFile=user_book1.db` なら、同じディレクトリの次のファイルをこの順に引きます。

```text
user_book1-000.db
user_book1-001.ybb
user_book1-002.db
...
user_book1.db
```

- **番号**: `000` から 1 ずつ探し、最初に欠けた番号で探すのをやめます (`000` が無ければ優先定跡は
  使いません)。3 桁に満たない番号は 0 で埋め、`1000` 以降はそのままの桁数です。
- **拡張子**: 番号ごとに `.db` と `.ybb` のどちらでもかまいません。両方あれば `BookFile` と同じ
  拡張子の方を使い、`info string priority book file exists twice. use : <パス>` を出します。
  `BookFile` が `.db` / `.ybb` で終わらなければ優先定跡は探しません。
- **引き方**: 先頭のファイルから順に局面を引き、局面が載っていない (手が 0 個の局面を含む) ときだけ
  次のファイルへ進みます。`FlippedBook=true` なら、ファイルごとに反転局面まで引いてから次のファイルへ
  進みます。当たったファイルで `BookEvalDiff` などのフィルタで候補が残らなくても、後ろのファイルは
  引きません。ponder の補完で子局面を引くときも同じ順です。手の行が無い `sfen` 行は、
  `BookOnTheFly=true` のテキスト `.db` と `.ybb` では「手が 0 個の局面」として次のファイルへ進み、
  丸読みのテキスト `.db` では局面として登録しないので反転局面を引きます (YaneuraOu と同じ)。
- **読み出しエラー (YaneuraOu と違う点)**: probe 中にファイルの読み出しエラーが起きたら、その局面は
  定跡なしとし、後ろのファイルは引きません。YaneuraOu は載っていないものとして次のファイルへ
  進みます。読めなかったファイルにその局面が載っていたか分からないので、優先度の低いファイルの手を
  選ぶと、前のファイルで手を上書きした意図に反し得るためです。読み込み時 (`isready`) の失敗は
  エラーの行で分かるので、そのファイルを飛ばして残りで続けます。
- **オプション**: `BookOnTheFly`、`IgnoreBookPly`、`FlippedBook`、`BookEvalDiff` などは全ファイルに
  共通です。
- **`isready`**: ファイルごとに `info string book loaded: ...` (丸読み) か
  `info string book opened on the fly: ...` を引く順に出します。読めなかったファイルはエラーを出して
  飛ばし、残りのファイルで続けます。優先定跡を足したり消したりすると、次の `isready` で読み直します。

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
