# bench_nnue_eval

NNUE の推論処理を ns/op で計測します。探索 NPS や棋力の測定ではありません。

```bash
cargo run -p tools --release --bin bench_nnue_eval -- \
  --nnue-file "$SHOGI_DATA/nnue/model.bin" --iterations 500000 --warmup 10000
```

モデルのアーキテクチャに対応する feature を有効にしてビルドしてください。

## full モード（既定）

内蔵の5局面を使い、次を別々に計測します。

- `refresh_ns`: 全局面を巡回する accumulator の refresh。
- `eval_ns`: **局面0（初期局面）固定**で準備済み accumulator から評価。計測前に独立した fresh evaluator と値を照合し、固定局面で warmup します。計測ループに refresh・モデル複製・割り当ては含みません。
- `total_ns`: 全局面を巡回し、各回で refresh + evaluate。上の2項の単純な和ではありません。
- `evals_per_sec`: refresh + evaluate の throughput（`1e9 / total_ns`）。

人向け表示の後に JSON を出します。既存の数値キーに加え、`eval_scope: "fixed-position"`、`eval_position_index: 0`、`eval_sfen` が eval-only の対象を記録します。
以前の eval-only は局面0の accumulator のまま異なる局面を渡していたため、この固定局面の測定値と同条件の指標として比較できません。過去の refresh + eval や探索 NPS まで一括で再解釈しないでください。

## モードとオプション

| オプション | 意味 |
|---|---|
| `--nnue-file` | 必須。モデルファイル |
| `--mode` | `full`（既定）、`layer-stack-propagate`、`layer-stack-eval`、`layer-stack-refresh-cache`、`layer-stack-update-cache` |
| `--iterations` | 反復数。既定500000。full では正数必須 |
| `--warmup` | 計測前の反復数。既定10000 |
| `--ls-bucket-mode` | LayerStacks の bucket 選択モード |
| `--ls-progress-coeff` | progresskpabs 係数。指定時は bucket 計算のマイクロベンチも実行 |
| `--ls-progress-buckets` | progresskpabs の推論 bucket 数。このモードでは必須 |

LayerStacks 専用モードは各局面に対応する入力・accumulator を事前準備する既存経路です。`layer-stack-propagate` は dense 部、`layer-stack-eval` は準備済み accumulator の評価、`layer-stack-refresh-cache` はキャッシュ付き refresh、`layer-stack-update-cache` は1手差分の更新を計測します。bucket 分布も出力します。full の固定局面 eval-only とは測定範囲が異なります。

比較時は実行バイナリの commit、build feature、CPU、モデル、routing 設定、モードと反復数を保存してください。
