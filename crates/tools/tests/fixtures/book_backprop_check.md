# 実 YaneuraOu との差分 fixture

`*_expected.db` は実 YaneuraOu の `makebook peta_shock` の出力をそのまま保存している。
比較対象は value のみ。YaneuraOu は depth 欄に選択手を辿った出口までの距離を書くが、
`book_backprop` は入力の depth / count / ponder を保つので、native 側ではその保存を別に確認する。

## `book_backprop_check`: 連続王手

`book_backprop_check_in.db` は飛車が 5c / 4c、玉が 5a / 4a を往復する合法な 4 局面。
王手側の離脱値は -120 / -80、被王手側の離脱値は -200。
入力探索 depth は意図的に異なる値にしてある。

王手側が -80 の離脱を選べるため、被王手側のループ手は +80 となる。
4c5c は同値の離脱 4c4d より優先しないよう -81 になる。
離脱のない純粋な連続王手（王手側 -32000 / 被王手側 +32000）はライブラリ単体テストでも確認する。

## `book_backprop_cycle_exit`: 根を通る通常の循環

`book_backprop_cycle_exit_in.db` は 8 局面。根 `k8/9/9/9/9/9/3r5/1G7/4K4 b` では
5i4i が値 85 の出口、8h7h が金と玉の往復で根へ戻る循環、5i5h が飛車の連続王手の循環に入る。

循環の途中で根へ戻れば先手は 5i4i の 85 を選べるので、8h7h は千日手の 0 ではなく 85 になり、
同値の出口より優先しないよう 84 になる。後手の 9a9b (-96) はこれより悪いので選ばれない。
5i5h は、後手が連続王手を続けると反則負けなので 6g6f (-50) で離脱し、先手から見て 50 になる。

## 生成手順

2026-10-23 に YaneuraOu `bd6db90a7ac91875356f6e4ac61a18c9031a9116` を
Visual Studio 2022 Build Tools で `Release-Material|x64` として build した実行ファイルで生成した。
USI 識別名は `YaneuraOu MaterialLv1 9.80git 64AVX2BMI2`、実行ファイルの SHA-256 は
`7b88e05487ee0345bacc94576fcb6f8b8478e4e42195f92f3a9f36fb00bba6cc`。
`peta_shock` の値は評価関数に依存しないので、評価関数の要らない Material 版を使う。
以下の `$FIXTURES` はこのディレクトリの絶対パス、`<name>` は fixture 名に置き換える。

```text
usi
setoption name BookDir value $FIXTURES
setoption name BookFile value no_book
setoption name FlippedBook value true
setoption name USI_Hash value 16
isready
makebook peta_shock <name>_in.db <name>_expected.db
quit
```

どちらも `both physical-side policies certified.` で保証検査を通過した。
