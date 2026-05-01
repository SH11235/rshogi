# コマンド例

> CSA 対局クライアント (`csa_client`) は独立 crate `rshogi-csa-client` に分離
> された。コマンド例とサンプル設定は
> [`crates/rshogi-csa-client/examples/README.md`](../../rshogi-csa-client/examples/README.md)
> を参照。

## トーナメント (tournament)

複数エンジン間の総当たり並列対局を実行し、JSONL形式で結果を出力する。

### 2エンジン比較（秒読み）

```bash
cargo run -p tools --release --bin tournament -- \
  --engine target/release/rshogi-usi --engine-label dev \
  --engine target/release/rshogi-usi --engine-label base \
  --games 200 --byoyomi 1000 --threads 1 --hash-mb 256 \
  --concurrency 8 \
  --engine-usi-option "0:EvalFile=eval/new_model.bin" \
  --engine-usi-option "1:EvalFile=eval/base_model.bin" \
  --startpos-file start_sfens_ply24.txt \
  --out-dir "runs/selfplay/$(date +%Y%m%d_%H%M%S)_dev_vs_base"
```

### rshogi vs YaneuraOu

```bash
cargo run -p tools --release --bin tournament -- \
  --engine target/release/rshogi-usi --engine-label rshogi \
  --engine /path/to/YaneuraOu-by-gcc --engine-label yaneuraou \
  --games 100 --byoyomi 500 --threads 1 --hash-mb 256 \
  --concurrency 8 \
  --usi-option "EvalFile=eval/halfkp_256x2-32-32_crelu/suisho5.bin" \
  --engine-usi-option "1:FV_SCALE=24" \
  --engine-usi-option "1:PvInterval=0" \
  --startpos-file start_sfens_ply24.txt \
  --out-dir "runs/selfplay/$(date +%Y%m%d_%H%M%S)_rs_vs_yo"
```

### 固定深さ対局

```bash
cargo run -p tools --release --bin tournament -- \
  --engine target/release/rshogi-usi --engine-label dev \
  --engine target/release/rshogi-usi --engine-label base \
  --games 100 --depth 10 --threads 1 --hash-mb 256 \
  --concurrency 8 \
  --out-dir "runs/selfplay/$(date +%Y%m%d_%H%M%S)_depth10"
```

## SPSA パラメータチューニング

詳細な手順書は [../docs/spsa_runbook.md](../docs/spsa_runbook.md) を参照。

### 1. canonical パラメータファイルの準備

`--init-from` に渡す canonical (起点) を用意する。渡せる形式は:

- rshogi デフォルト値 (`generate_spsa_params` で生成)
- rshogi 形式の既存 .params (過去のチューニング結果など)
- YaneuraOu 形式の既存 .params (YO 駆動時、または `yo_to_rshogi_params` 経由で rshogi 形式に変換したもの)

rshogi デフォルト値から始める場合の生成例:

```bash
cargo run --release -p tools --bin generate_spsa_params -- \
  --output spsa_params/canonical.params
```

### 2. SPSA 実行

```bash
RUN_DIR="runs/spsa/$(date +%Y%m%d_%H%M%S)"

cargo run --release -p tools --bin spsa -- \
  --run-dir "${RUN_DIR}" \
  --init-from spsa_params/canonical.params \
  --total-pairs 6400 \
  --batch-pairs 32 \
  --concurrency 8 \
  --seed 1 \
  --startpos-file start_sfens_ply24.txt \
  --threads 1 --hash-mb 256 --byoyomi 1000
```

v4 仕様:
- `--total-pairs N`: SPSA 全体の game pair 数 (= fishtest `num_iter`)。total_games = 2N
- `--batch-pairs B`: 1 batch あたりの game pair 数。1 batch で `2B` 局を消化し θ を 1 回更新
- `--seed S`: 単一 base_seed (省略時はランダム)。multi-seed 機能 (`--seeds` /
  `--parallel-seeds`) は撤去された。複数 seed 比較は **`--seed` を変えた独立
  run dir** で並列実行する

`<run-dir>` 配下に `state.params` / `final.params` / `meta.json` /
`values.csv` / `stats.csv` が自動生成される。CSV のパスを別途指定したい場合は
`--stats-csv` / `--param-values-csv` で個別 override 可能。

### 3. 途中から再開

```bash
cargo run --release -p tools --bin spsa -- \
  --run-dir "${RUN_DIR}" \
  --init-from spsa_params/canonical.params \
  --resume \
  --total-pairs 12800 \
  --batch-pairs 32 \
  --concurrency 8 \
  --seed 1 \
  --startpos-file start_sfens_ply24.txt \
  --threads 1 --hash-mb 256 --byoyomi 1000
```

`--total-pairs` を当初値より大きくして batch を継ぎ足したい場合は
`--force-schedule` を併用する (`--batch-pairs` の途中変更は k 軸の不整合に
なるため非推奨)。

`--init-from` を resume 時にも指定すると、起動時に canonical との整合性
diagnostic が出る (乖離が閾値超過したら `--strict-init-check` で error 化可能)。

### 4. 結果の確認

```bash
# デフォルト値との差分表示
cargo run -p tools --bin spsa_param_diff -- \
  --current "${RUN_DIR}/state.params"

# 特定グループのみ表示
cargo run -p tools --bin spsa_param_diff -- \
  --current "${RUN_DIR}/state.params" \
  --regex "CORR"

# パラメータ値の推移を含めた分析
cargo run -p tools --bin spsa_param_diff -- \
  --current "${RUN_DIR}/state.params" \
  --param-values-csv "${RUN_DIR}/values.csv"
```

### 5. 統計データの可視化用CSV変換

```bash
cargo run -p tools --bin spsa_stats_to_plot_csv -- \
  "${RUN_DIR}/stats.csv" \
  --output-csv "${RUN_DIR}/plot.csv" \
  --window 16
```

複数 run の比較 (例 `--seed` を変えた独立 run dir 群) は、各 run の
`stats.csv` を pandas/awk で concat してから集計する (v4 では
`stats_aggregate.csv` の自動生成は撤去された)。

## 自己対局 (engine_selfplay)

### 基本（学習データ生成）

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 100 --byoyomi 1000 --threads 4 --hash-mb 512
```

### 学習データなしで対局のみ

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 10 --byoyomi 500 --threads 4 --hash-mb 512 \
  --no-training-data
```

### 異なるNNUEモデル同士の比較

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 50 --byoyomi 500 --threads 4 --hash-mb 512 \
  --usi-options-black "EvalFile=./model_a.nnue" \
  --usi-options-white "EvalFile=./model_b.nnue" \
  --no-training-data
```

## 学習データ処理

### シャッフル

```bash
cargo run -p tools --release --bin shuffle_psv -- \
  --input data.psv --output shuffled.psv
```

### 再評価（rescore）

```bash
cargo run -p tools --release --bin rescore_psv -- \
  --input data.psv --output rescored.psv \
  --nnue model.nnue --use-qsearch --threads 8
```

### 内容確認（JSONL変換）

```bash
cargo run -p tools --release --bin psv_to_jsonl -- \
  --input data.psv --output data.jsonl --limit 100
```

## ベンチマーク

### 内部APIモード

```bash
cargo run -p tools --release --bin benchmark -- --internal
```

### マルチスレッドスケーリング測定

```bash
cargo run -p tools --release --bin benchmark -- \
  --internal --threads 1,2,4,8
```
