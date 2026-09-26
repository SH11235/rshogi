# rshogi-book

`rshogi-book` は YANEURAOU-DB2016 テキスト `.db` 形式の定跡を読み込み、root 局面で
定跡手を 1 手 probe する crate です。USI 統合では `rshogi-usi` の Book 系オプションが
`BookOptions` に反映されます。

## 定跡 probe オプション

| USI オプション | 既定 | 説明 |
| --- | --- | --- |
| `USI_OwnBook` | `true` | 定跡使用の総合スイッチ。`false` なら probe しない。 |
| `BookFile` | `no_book` | 定跡ファイル名。`no_book` または空なら定跡をロードしない。 |
| `BookExploreFile` | 空文字列（無効） | 指定局面の定跡手を等確率で選ぶ UTF-8 ファイル。 |
| `BookDir` | `book` | 相対 `BookFile` を解決するディレクトリ。 |
| `BookMoves` | `16` | この手数まで定跡を使う。 |
| `BookEvalDiff` | `30` | 候補中の最大 value からこの差分以内の手だけ残す。 |
| `BookEvalBlackLimit` | `0` | 先手番で採用する value 下限。 |
| `BookEvalWhiteLimit` | `-140` | 後手番で採用する value 下限。 |
| `BookDepthLimit` | `0` | 筆頭手の depth 下限。`0` は無効。 |
| `NarrowBook` | `false` | count 情報がある場合、出現率 10% 未満の手を除外する。 |
| `BookSelectValue` | `false` | 評価値フィルタ後の生存候補から value 最大手を決定的に選ぶ。同値は count 大、さらに同値なら USI 昇順。`true` のとき `NarrowBook` / `ConsiderBookMoveCount` / 等確率抽選より優先する。 |
| `ConsiderBookMoveCount` | `false` | `true` なら count 比例抽選する。全 count が 0 の場合は等確率。 |
| `IgnoreBookPly` | `false` | ロード時に SFEN の手数を無視してキーを正規化する。 |
| `FlippedBook` | `true` | miss 時に先後反転局面で再検索する。 |

選択順序は、合法性検証、`BookDepthLimit`、`BookEvalDiff` / value 下限フィルタの後に
`BookSelectValue` を評価します。`BookSelectValue=false` の場合は従来どおり
`NarrowBook` を適用し、その後 `ConsiderBookMoveCount` または等確率抽選で選びます。

## 局面を指定した定跡手の乱択

`setoption name BookExploreFile value book/explore.txt` で指定し、`isready` で読み込みます。
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
