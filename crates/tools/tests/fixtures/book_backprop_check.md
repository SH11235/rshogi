# 連続王手の実エンジン差分 fixture

`book_backprop_check_in.db` は飛車が 5c / 4c、玉が 5a / 4a を往復する合法な 4 局面。
王手側の離脱値は -120 / -80、被王手側の離脱値は -200。
`book_backprop_check_expected.db` は実 YaneuraOu の出力をそのまま保存している。
入力探索 depth は意図的に異なる値にしてあり、比較対象は value のみ。
native 側では別途、入力 depth / count / ponder の保存も確認する。

2026-10-04 に makebook 対応の YaneuraOu `yo-lazy.exe` で生成した。
USI 識別名は `YaneuraOu NNUE 9.80git 64AVX512VNNI TOURNAMENT`、実行ファイルの
SHA-256 は `57c541b288fd9d07528d77da920ee97c1718fa9403b9c289ec6f339ebfc71812`。
実行時の結果は `check-loops nodes1 : 2`、`check-loops nodes2 : 4`、
`converged_moves : 0`。以下の `$FIXTURES` はこのディレクトリの絶対パス、
`$EVAL_DIR` は実エンジンが読める評価関数のディレクトリに置き換える。

```text
usi
setoption name EvalDir value $EVAL_DIR
setoption name FV_SCALE value 14
setoption name BookDir value $FIXTURES
setoption name BookFile value no_book
setoption name FlippedBook value true
setoption name USI_Hash value 16
isready
makebook peta_shock book_backprop_check_in.db book_backprop_check_expected.db
quit
```

王手側が -80 の離脱を選べるため、被王手側のループ手は +80 となる。
4c5c は同値の離脱 4c4d より優先しないよう -81 になる。
離脱のない純粋な連続王手（王手側 -32000 / 被王手側 +32000）はライブラリ単体テストでも確認する。
