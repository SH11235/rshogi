# コマンド例

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

### NNUE vs Material評価の比較

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 50 --byoyomi 500 --threads 4 --hash-mb 512 \
  --usi-options-black "EvalFile=./model.nnue" \
  --usi-options-white "MaterialLevel=9" \
  --no-training-data
```

### 長時間対局（持ち時間制）

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 10 --threads 4 --hash-mb 1024 \
  --btime 300000 --wtime 300000 \
  --binc 5000 --winc 5000 \
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
