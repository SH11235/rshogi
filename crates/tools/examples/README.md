# コマンド例

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

詳細な手順書は [docs/spsa_runbook.md](../../../docs/spsa_runbook.md) を参照。

### 1. パラメータファイル生成

SearchTuneParams の全 SPSA パラメータを `.params` ファイルに書き出す。

```bash
RUN_DIR="runs/spsa/$(date +%Y%m%d_%H%M%S)"
mkdir -p "${RUN_DIR}"

cargo run --release -p tools --bin generate_spsa_params -- \
  --output "${RUN_DIR}/tuned.params"
```

### 2. SPSA 実行

```bash
cargo run --release -p tools --bin spsa -- \
  --params "${RUN_DIR}/tuned.params" \
  --iterations 200 \
  --games-per-iteration 64 \
  --concurrency 8 \
  --seeds 1,2,3,4 \
  --startpos-file start_sfens_ply24.txt \
  --threads 1 --hash-mb 256 --byoyomi 1000 \
  --stats-csv "${RUN_DIR}/stats.seed.csv" \
  --stats-aggregate-csv "${RUN_DIR}/stats.aggregate.csv" \
  --param-values-csv "${RUN_DIR}/param_values.csv"
```

### 3. 途中から再開

```bash
cargo run --release -p tools --bin spsa -- \
  --params "${RUN_DIR}/tuned.params" \
  --resume \
  --iterations 400 \
  --games-per-iteration 64 \
  --concurrency 8 \
  --seeds 1,2,3,4 \
  --startpos-file start_sfens_ply24.txt \
  --threads 1 --hash-mb 256 --byoyomi 1000 \
  --stats-csv "${RUN_DIR}/stats.seed.csv" \
  --stats-aggregate-csv "${RUN_DIR}/stats.aggregate.csv" \
  --param-values-csv "${RUN_DIR}/param_values.csv"
```

### 4. 結果の確認

```bash
# デフォルト値との差分表示
cargo run -p tools --bin spsa_param_diff -- \
  --current "${RUN_DIR}/tuned.params"

# 特定グループのみ表示
cargo run -p tools --bin spsa_param_diff -- \
  --current "${RUN_DIR}/tuned.params" \
  --regex "CORR"

# パラメータ値の推移を含めた分析
cargo run -p tools --bin spsa_param_diff -- \
  --current "${RUN_DIR}/tuned.params" \
  --param-values-csv "${RUN_DIR}/param_values.csv"
```

### 5. 統計データの可視化用CSV変換

```bash
cargo run -p tools --bin spsa_stats_to_plot_csv -- \
  "${RUN_DIR}/stats.aggregate.csv" \
  --output-csv "${RUN_DIR}/plot.csv" \
  --window 16
```

### 6. shogitest 連携

```bash
# tuned.params を shogitest の option 形式に変換
cargo run -p tools --bin params_to_shogitest_options -- \
  "${RUN_DIR}/tuned.params" --one-per-line
```

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
cargo run -p tools --release --bin shuffle_pack -- \
  --input data.pack --output shuffled.pack
```

### 再評価（rescore）

```bash
cargo run -p tools --release --bin rescore_pack -- \
  --input data.pack --output rescored.pack \
  --nnue model.nnue --use-qsearch --threads 8
```

### 内容確認（JSONL変換）

```bash
cargo run -p tools --release --bin pack_to_jsonl -- \
  --input data.pack --output data.jsonl --limit 100
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
