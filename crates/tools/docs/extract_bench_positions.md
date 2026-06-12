# extract_bench_positions

`extract_bench_positions` は、floodgate CSA 棋譜と selfplay JSONL から、教師モデルのラベル品質を局面クラス別に測るためのベンチ局面を抽出するツールです。

## 使い方

```bash
cargo run -p tools --bin extract_bench_positions -- \
  --csa-dir data/floodgate/raw \
  --jsonl 'runs/selfplay/**/*.jsonl' \
  --out-dir runs/label_bench \
  --min-rating 3000 \
  --per-cell 200 \
  --seed 1
```

`--csa-dir` はディレクトリまたは glob を複数指定できます。`--jsonl` も glob を複数指定できます。

## 出力

- `label_bench.jsonl`: `progress_band`, `eval_band`, `nyugyoku`, `in_check`, `stm` のセルごとに層化サンプリングした局面。
- `label_bench_nyugyoku.jsonl`: `%KACHI` 終局、またはいずれかの玉が敵陣へ入った対局 (floodgate / selfplay とも) の全局面オーバーサンプル。
- `startpos_ply100_balanced.txt`: 素の SFEN 1 行形式の開始局面リスト (`data/floodgate/floodgate_r3900_*.txt` と同形式)。既定では ply 100±4、かつ `|eval_cp_black| <= 150`、対局ごとに 1 局面、SFEN 重複排除。
- `stats.json`: 母集団数、採択数、ソース別対局数、終局種別、CSA 評価値符号検証の要約。

レコードには `declarable` (手番側が 27 点法で宣言勝ち可能か、`Position::declaration_win` による) も含まれます。

## クラス定義

`progress_band`:

- `1-40`
- `41-80`
- `81-120`
- `121+`

`eval_band` は黒視点 cp の絶対値で分類します。

- `0-150`
- `151-600`
- `601-1500`
- `1501+`
- `mate`: `abs(eval_cp_black) >= 30000`
- `unknown`: 評価値なし

`nyugyoku`:

- `none`: どちらの玉も敵陣三段目以内にいない
- `black_entered`: 先手玉のみ敵陣三段目以内
- `white_entered`: 後手玉のみ敵陣三段目以内
- `both_entered`: 両玉が敵陣三段目以内

`black_points` / `white_points` は CSA 27 点法と同じ駒点定義で、敵陣内の自駒と持ち駒を数えます。玉は点数に含めず、大駒は 5 点、小駒は 1 点です。

## CSA 評価値

CSA の評価コメントは指し手直後の `'** <cp> ...` を読みます。floodgate の評価値は**手番によらず常に先手視点**であり、そのまま `eval_cp_black` に使います。

この規約は `stats.json` の `sign_validation` で実証確認できます: `%TORYO` 対局の最終評価値を勝敗と突き合わせ、「先手視点」仮説 (`agree_black_view`) と「指し手側視点」仮説 (`agree_mover_view`) の一致数を両方記録します。floodgate r3000+ 約 1,750 局では先手視点が 100%、指し手側視点は約 54% (≒先手勝率、つまり無相関) でした。

## CSA 不成への対応

YO 準拠の合法手生成は歩・大駒などの不成を生成しないため、AobaZero 等が指す不成は合法手照合に失敗します。その場合は USI 表記へ直接変換し、`pseudo_legal_with_all` で検証して受理します。
