# spsa

USIエンジンのパラメータを対局結果から調整する。起点を `--init-from`、作業領域を
`--run-dir` に分ける。ビルド、対局条件、再開、停止は [実行runbook](spsa_runbook.md) を参照。

## 更新方式

- `--dispatch batch` (既定): `--batch-pairs` 個の pair を同じ摂動で指し、全局の完了後に 1 回更新する。
- `--dispatch async`: pair ごとに独立な摂動で指し、結果の到着順に dispatch 時の係数で更新する。
  lag の上限、失敗した pair の指し直し、journal と snapshot による再開を持つ。
  [runbook §15](spsa_runbook.md#15-非同期-per-pair-更新---dispatch-async) を参照。

## 棋譜と摂動の記録

既定で、確定した結果に対応する全局の棋譜を `<run-dir>/games.jsonl` に、batch モードでは batch ごとの摂動を
`<run-dir>/batches.jsonl` にも追記する。`--no-game-records` で両方を書かない。有無は meta に保存し、
resume で切り替えると拒否する (切り替えると確定した局の棋譜が欠けるため)。記録機能より前の batch の meta には
この項目が無く、resume 以降の batch から記録する。

### games.jsonl (1 局 1 行、schema `spsa_game` version 1)

| 項目 | 意味 |
|---|---|
| `schema` / `version` | `"spsa_game"` / `1` |
| `mode` | `"batch"` / `"async"` |
| `pair_id` | run 全体の pair 番号 (0 始まり) |
| `pair_slot` | 0 = plus が先手の局、1 = plus が後手の局 |
| `attempt` | async のみ。journal の `dispatch` / `received` の attempt |
| `iteration` | batch のみ。batch 番号 (1 始まり) |
| `dispatch_k` | async のみ。係数 (c_k, r_k) を決めた時点の完了 pair 数 |
| `game_id` | engine に渡した対局番号 |
| `plus_is_black` | plus 側が先手か |
| `startpos_idx` | 開始局面ファイルを読み込んだ並びでの index |
| `startpos_sfen` | 開始局面ファイルの手順を適用した後の SFEN。`moves` はこの局面から指す |
| `outcome` | `black_win` / `white_win` / `draw` |
| `plus_score` | plus 視点の 1 / 0 / -1 |
| `reason` | 終局理由 (`resign` / `win` / `timeout` / `illegal_move` / `no_bestmove` / `max_moves` / 千日手系) |
| `plies` | 下の配列の長さ |
| `retries` | この結果を得るまでに engine の通信障害で指し直した回数 |
| `moves` | 指し手 (USI) |
| `elapsed_ms` | 各手の消費時間 (spsa 側の計測) |
| `score_cp` / `score_mate` | 評価値 (手番側視点)。score の無い手は両方 null |
| `score_bound` | `lowerbound` / `upperbound`。score があって null なら上下界の指定なし |
| `depth` / `nodes` | 探索深さ・nodes |

- 手ごとの値は `moves` と同じ長さの配列で、i 番目が i+1 手目に対応する。`score_cp` 以降の列は全要素 null なら省くので、
  欠けた列は長さ `plies` の null 列として読む。評価値・深さ・nodes は主 PV の info を項目ごとに上書きした最後の値で、
  同じ 1 行に揃った値とは限らない。距離不明の `score mate +/-` は `score_cp`・`score_mate` とも null になる (rshogi は常に距離付きで出す)。
- `moves` の最後の要素は `resign` / `win` / `illegal` / `none` になり得る。`reason` が `timeout` のときは最後の要素が
  指し手の形でも盤面に適用されていない。
- tournament の JSONL (analyze_selfplay が読む) とは形式が違う。項目名は tournament の result 行に揃えている。

### batches.jsonl (batch のみ、1 batch 1 行、schema `spsa_batch` version 1)

`iteration`・`batch_pairs`・`k_pair_start` (係数を決めた時点の完了 pair 数) と、active param を params の順に並べた
`params` (名前)・`c_k`・`r_k`・`flip` (±1)・`plus`・`minus` を持つ。`plus` / `minus` は丸め・clamp 後の確定値で、
engine へ送る文字列と同じ桁に揃えている (mapping の名前・符号の変換前の値)。

async では摂動を別ファイルに書かない。journal の `dispatch` 行の `flips`・`plus`・`minus` が active param の確定値で、
名前は gen0 snapshot (`snapshots/snapshot-000000.json`) の `definitions` を保存順のまま `active` のものだけ抜いた並びに対応する。
c_k・r_k は meta の schedule と gen0 の c_end・r_end から求める。

### 書き込みの順序

- batch: batch の全局が終わったら、checkpoint (stats → state → values → meta) より前に、その batch の全局と batch の
  1 行を書いて fsync する。batch の途中で止まったら、その batch の分は書かない。
- async: pair の 2 局が揃い、それが実行中の現 attempt の結果なら、journal の `received` より前に 2 行を書いて fsync する。
  `duplicate` になる結果は書かない。書き込みに失敗したら `received` も snapshot も書かずに終了する。

どちらも棋譜を checkpoint より先に durable にするので、checkpoint で確定した局の棋譜は欠けない。逆に、棋譜を書いた後
checkpoint の前に落ちると、確定していない行が残る。spsa は既存の行を消さない (追記転送で回収できるように)。

### 読み方 (重複と未確定の行の除き方)

1. 改行で終わらない行・JSON として読めない行は捨てる (書き込み途中で落ちた末尾。resume では改行だけ補うので次の行とは連結しない)。
2. async: journal の `received` がある pair だけを確定とし、`(pair_id, attempt, pair_slot)` が `received` の attempt と一致する行を
   採る。同じ組が複数あれば後の行。`plus_is_black`・`plus_score`・`reason`・`plies` は `received` の `games` と一致する。
   θ の更新に使った局だけを見たいときは、さらに `accept` がある pair に絞る。`received` の前に落ちた pair は resume で attempt を
   上げて指し直すので、古い attempt の行は孤立して残る。
3. batch: `iteration <= meta.completed_iterations` の行を `(iteration, pair_id, pair_slot)` ごとに後の行で採る。batches.jsonl は
   `iteration` ごとに後の行。捨てた batch を resume で指し直すと同じ番号の行が後に追記される (開始局面や確定値が同じとは限らない)。
4. 確定した pair の 2 行が揃わなければ、回収の途中なら未回収、最終の確認では異常として扱う。async の resume は journal の
   `received` ごとに 2 行があるかを確かめ、欠けていれば警告する (run は止めない)。

記録するのは確定した結果に対応する局だけで、通信障害で捨てた attempt の局 (第 2 局で失敗した pair の第 1 局を含む) は書かない。
1 局はおよそ 3〜5 KB (120 手、評価値・深さ・nodes の列あり)。

## 入力形式

パラメータ行より前に、次の形式宣言を1回だけ置く。

```text
# spsa-format: c-end-r-end-v1
name,int,10,0,100,5,0.002
```

列は `name,type,value,min,max,c_end,r_end`。`c_end` は終端摂動幅、`r_end` は
終端学習率係数であり、旧形式の `step,delta` と同じ意味ではない。
空行、`#` コメント、行末の `//` コメント、`[[NOT USED]]` は利用できる。
生成される `state.params` と `final.params` にも形式宣言が付く。

宣言なし、未知の宣言、重複・行途中の宣言は既定で拒否する。列7が小さいことだけでは
現形式と判定できないため、数値による自動判別や自動補正はしない。

`--init-from` の全行とengine mappingは、`--force-init` による既存成果物の変更前に
検証する。非有限値、数値の構文・範囲不正、名前や翻訳先の重複、空の入力は拒否し、
既存のstate・meta・CSV・finalを保持する。`--meta-file` の指定先も同様に保持する。

## 既存ファイルの利用

列6/7が既に `c_end/r_end` と確認できた未宣言ファイルには、
`--assume-c-end-r-end` を明示できる。このフラグは `--init-from` と既存
`state.params` の未宣言入力だけに適用され、数値や元ファイルを変更しない。
未知・旧形式を宣言しているファイルは、このフラグを指定しても拒否する。

既存runを再開するときは、`state.params` と `meta.json` を手編集せず
`--resume --assume-c-end-r-end` を使う。通常のハッシュ・schedule検査は引き続き有効。
次のcheckpointから形式宣言が保存され、ハッシュも通常どおり更新される。

旧 `step,delta` ファイルはこのフラグで読み替えない。値列を新canonicalへ移し、
`c_end/r_end` を明示的に設計し直して新runを開始する。
[形式と移行の詳細](spsa_runbook.md#7-params-ファイルの形式と移行) を参照。
