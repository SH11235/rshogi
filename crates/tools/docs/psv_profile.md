# psv_profile

PSV 教師データの手数・入玉・宣言・勝敗の分布を集計します。シャッフル前の棋譜順の PSV では、
対局ごとの終局 (宣言勝ちかどうか) も集計します。入力は読むだけで、書き換えません。

```bash
cargo run --release -p tools --bin psv_profile -- \
  --games --ply-bin 20 --threads 8 --json profile.json \
  "$SHOGI_DATA"/teachers/<dataset>/*.bin
```

## オプション

| オプション | 既定 | 意味 |
| --- | --- | --- |
| `<inputs>...` | (必須) | 入力 PSV。ファイルごとに独立して読む (対局はファイルをまたがない) |
| `--games` | off | 棋譜順の PSV として対局に分け、終局を集計する |
| `--ply-bin N` | 10 | 手数の集計幅 (1 以上) |
| `--entering-king-rule` | `CSARule27` | 宣言の判定に使うルール。USI の `EnteringKingRule` と同じ値 (`CSARule24` / `CSARule27` / `CSARule24H` / `CSARule27H` / `TryRule` / `NoEnteringKing`) |
| `--json PATH` | なし | 集計結果を JSON で保存する。入力と同じ実体 (同一パス・hardlink・symlink) は拒否する |
| `--threads N` | 0 (全コア) | 局面の展開と判定の並列数 |

入力のサイズが 40 byte の倍数でない場合は停止します。chunk (26 万局面) ずつ読むので、
ピークメモリは入力の件数によりません。

## 局面の集計

手数 (`gamePly`) の帯ごとと全体で、次を数えます。

| 列 | 意味 |
| --- | --- |
| `entered%` | 入玉の局面。先手玉が 1〜3 段目、または後手玉が 7〜9 段目 |
| `black%` / `white%` / `both%` | 入玉の内訳 (先手玉だけ・後手玉だけ・両方) |
| `advancing%` | 入玉ではなく、どちらかの玉が 4〜6 段目 ([`psv_gate_by_king_zone`](psv_gate_by_king_zone.md) の `advancing` と同じ) |
| `declarable` | 手番側がその局面で宣言できる (`--entering-king-rule` の条件をすべて満たす) 局面の数 |
| `win%` / `draw%` / `loss%` | `game_result` (手番側から見た勝敗) |

`continued` は、直前の record と手数が 1 違いで勝敗が反転している record の割合です。棋譜順のまま並んでいる
PSV では 100% に近く、シャッフル済みの PSV では 0% に近くなります。`--games` を付けるかどうかの目安にしてください。

## 対局の集計 (`--games`)

直前の record と手数が 1 違いで勝敗が反転していれば同じ対局、そうでなければ新しい対局とみなします。
シャッフル済みの PSV に付けると、意味のない集計になります。

対局の最後の局面 (手番側から見た勝敗を `r` とする) から、終局を次のように分けます。

| 列 | 判定 |
| --- | --- |
| `decl_marked` | `r` が勝ちで、bestmove が YaneuraOu の `MOVE_WIN` (move16 = `0x183`) |
| `decl_now` | `r` が勝ちで、手番側がその局面で宣言できる |
| `decl_next` | `r` が負けで、記録された bestmove を指すと相手が宣言できる局面になる |
| `other_decisive` | 上のどれでもない勝敗 (投了・詰み・最大手数での勝敗の割当など) |
| `draw%` | `r` が引き分け |

YaneuraOu 系の生成ツールは宣言できる局面を書かずに対局を終えることが多く、その場合は `decl_next` に数えられます。
宣言勝ちの判定は記録された bestmove が正しく指されたという前提で、bestmove が非合法なら `decl_next` にしません。

対局の行は最後の局面の手数で分け、`entered_games%` は入玉の局面 (先後どちらか) を 1 つ以上含む対局、
`black_win%` / `white_win%` は先手から見た勝敗です。`start_ply` は最初の局面の手数で分けた対局数です。

## JSON

`--json` の出力は `inputs`・`entering_king_rule`・`ply_bin` と、`profile` (`records`・`continued_records`・`totals`・
`by_ply`・`games`) を持ちます。`by_ply[i]` と `games.by_end_ply[i]`・`games.by_start_ply[i]` は手数
`i * ply_bin` から `(i + 1) * ply_bin - 1` の帯で、件数はすべて実数です。`--games` を付けないと `games` は `null` です。
