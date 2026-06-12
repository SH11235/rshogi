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
- `label_bench_nyugyoku.jsonl`: `%KACHI` 終局かついずれかの玉が敵陣へ入った floodgate 対局の入玉オーバーサンプル。
- `startpos_ply100_balanced.txt`: `startpos` / `startpos moves ...` 形式の開始局面リスト。既定では ply 100±4、かつ `|eval_cp_black| <= 150`。
- `stats.json`: 母集団数、採択数、ソース別対局数、終局種別、CSA 評価値符号検証の要約。

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

CSA の評価コメントは指し手直後の `'** <cp> ...` を読みます。評価値は指し手側視点として扱い、黒視点 cp へ正規化します。

- 先手指し手直後: `eval_cp_black = cp`
- 後手指し手直後: `eval_cp_black = -cp`

`%TORYO` 対局では、直前評価値が投了側視点で負になっているかを `stats.json` の `sign_validation` に記録します。
