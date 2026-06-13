# extract_bench_positions

`extract_bench_positions` は、floodgate CSA 棋譜と selfplay JSONL から、教師モデルのラベル品質を局面クラス別に測るためのベンチ局面を抽出するツールです。

## ビルド

```bash
cargo build -p tools --bin extract_bench_positions --release
# → target/release/extract_bench_positions
```

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

`--csa-dir` はディレクトリ（再帰探索）または glob を複数指定できます。`--jsonl` も glob を複数指定できます。

## オプション

| フラグ | 既定 | 説明 |
|---|---|---|
| `--csa-dir <PATH/GLOB>` | — | floodgate CSA のディレクトリまたは glob（複数可） |
| `--jsonl <GLOB>` | — | selfplay JSONL の glob（複数可） |
| `--out-dir <DIR>` | （必須） | 出力ディレクトリ |
| `--min-rating <u32>` | 3000 | floodgate 両者に要求する最小レート（不明レートは除外） |
| `--per-cell <usize>` | 200 | `label_bench` の層化セルあたり採択数 |
| `--nyugyoku-max <usize>` | 50000 | 入玉オーバーサンプルの最大局面数 |
| `--startpos-eval-abs-max <i32>` | 150 | `startpos` 出力に許す絶対評価値上限 |
| `--startpos-ply <u32>` | 100 | `startpos` 出力の中心 ply |
| `--startpos-window <u32>` | 4 | `startpos` 出力の ply 窓幅 |
| `--seed <u64>` | 1 | 決定的サンプリング用 seed |

## 出力

- `label_bench.jsonl`: `progress_band`, `eval_band`, `nyugyoku`, `in_check`, `stm` のセルごとに層化サンプリングした局面。
- `label_bench_nyugyoku.jsonl`: `%KACHI` 終局、またはいずれかの玉が敵陣へ入った対局 (floodgate / selfplay とも) の全局面オーバーサンプル。入玉**局面**だけではなく入玉対局の序中盤も含む全局面なので、多くのレコードは `nyugyoku == "none"` になる。実際に玉が敵陣にいる局面だけが欲しい場合は `nyugyoku != "none"` で絞る。
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

評価値は「指し手側がその局面を探索して報告した探索値」なので、**指し手前の局面**に対応付けます (PSV の `score` フィールドと同じ規約: 局面 + その局面の探索値 + そこで指された手)。

この規約は `stats.json` の `sign_validation` で実証確認できます: `%TORYO` 対局の最終評価値を勝敗と突き合わせ、「先手視点」仮説 (`agree_black_view`) と「指し手側視点」仮説 (`agree_mover_view`) の一致数を両方記録します。floodgate r3000+ 約 1,750 局では先手視点が 100%、指し手側視点は約 54% (≒先手勝率、つまり無相関) でした。

## CSA 不成への対応

YO 準拠の合法手生成は歩・大駒などの不成を生成しないため、AobaZero 等が指す不成は合法手照合に失敗します。その場合は USI 表記へ直接変換し、`pseudo_legal_with_all` で検証して受理します (棋譜由来の手なので自玉の王手放置は実在しないという前提)。

## 決定性とメモリ

サンプリングは seed 固定の reservoir sampling (Algorithm R) です。入力順 (CSA はパスソート順、JSONL は game_id ソート順) が同じなら、**同一 `--seed` で全出力が bit 一致**します。

全局面を貯めずにストリームで間引くため、**ピークメモリは入力件数に非依存**で、おおむね `セル数 × per_cell + nyugyoku_max + 互角局面数` の上限で頭打ちになります。数千万〜億局面規模の棋譜を流しても数十 MB 級に収まります。
