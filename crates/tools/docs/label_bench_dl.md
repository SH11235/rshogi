# label_bench_dl

`label_bench_dl` は、ground truth ラベラー (`label_bench_positions` / `extract_bench_positions`) が出した `label_bench` 形式の jsonl の各局面を **DL水匠 (標準 dlshogi ONNX)** の value head で静的評価し、評価値フィールド (`eval_dl`、既定) を追記するツールです。

後段で ground truth の `eval_deep` とクラス別 (`progress_band` / `eval_band` / `nyugyoku` 等) に突き合わせ、DL モデルのラベル品質を測るための入力を作ります。

## ビルド

ONNX 推論は `dlshogi-onnx` feature を有効にしたビルドでのみ動きます。feature 無しでもコンパイルは通りますが、実行すると再ビルド手順を案内して終了します。

```bash
cargo build --release -p tools --features dlshogi-onnx --bin label_bench_dl
# → target/release/label_bench_dl
```

## 実行環境 (ONNX Runtime)

GPU 推論には GPU 版 ONNX Runtime と CUDA / cuDNN（TensorRT 使用時は TensorRT も）が必要です。ライブラリは `ORT_DYLIB_PATH` / `LD_LIBRARY_PATH` で明示します（`rescore_psv` と同じ）。

```bash
export ORT_DYLIB_PATH=/path/to/onnxruntime-linux-x64-gpu-<ver>/lib/libonnxruntime.so
export LD_LIBRARY_PATH=/path/to/onnxruntime/lib:/usr/local/cuda/lib64:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
```

`ort` クレート (`api-24`) は ONNX Runtime 1.24 系を想定します。ONNX Runtime のバージョンと CUDA / cuDNN のバージョンは整合させてください（例: ORT 1.24 系は CUDA 12 + cuDNN 9）。`ORT_DYLIB_PATH` 未設定時はハングを避けるためエラーで停止します。

## 使い方

```bash
cargo run --release -p tools --features dlshogi-onnx --bin label_bench_dl -- \
  --in runs/label_bench/<date>/label_bench.jsonl \
  --out runs/label_bench/<date>/label_bench.dl.jsonl \
  --dlshogi-onnx-model /path/to/DL_suisho.onnx \
  --onnx-gpu-id 0 \
  --onnx-batch-size 1024
```

FP16 推論は `--onnx-tensorrt` を付けます（初回はエンジンコンパイルに時間がかかります。`--onnx-tensorrt-cache <DIR>` でキャッシュ可）。

## オプション

| フラグ | 既定 | 説明 |
|---|---|---|
| `--in <FILE>` | （必須） | 入力 jsonl（各行 JSON object、`sfen` フィールド必須） |
| `--out <FILE>` | （必須） | 出力 jsonl |
| `--out-field <STR>` | `eval_dl` | 追記するフィールド名 |
| `--dlshogi-onnx-model <PATH>` | （必須） | 標準 dlshogi ONNX モデルのパス |
| `--onnx-tensorrt` | false | TensorRT EP (FP16) を使う。未指定なら CUDA EP (FP32) |
| `--onnx-tensorrt-cache <PATH>` | — | TensorRT エンジンキャッシュの保存先 |
| `--onnx-batch-size <usize>` | 1024 | 1 回の推論あたりの最大局面数 |
| `--onnx-gpu-id <i32>` | 0 | CUDA device id（負値で CPU 推論） |
| `--onnx-eval-scale <f32>` | 600 | winrate→cp 変換スケール |

## 出力

入力 jsonl の各 object の全フィールドを保持し（入力順も保持）、`--out-field`（既定 `eval_dl`）を追記して 1 行ずつ書き出します。

`eval_dl` は **先手視点 cp** です。value head の勝率を winrate→cp 変換して手番 (STM) 視点 cp を得たのち、SFEN の手番が後手なら符号を反転して `eval_deep` / `eval_cp_black` と同じ先手視点規約に揃えています（手番は JSON の `stm` ではなく SFEN を正とします）。

パース失敗・`sfen` 不正・非 object 行は出力せず stderr に件数を記録します（`parse_errors` / `sfen_errors` / `non_object`）。

## モデル形式の注意

このツールは **標準 dlshogi (DL水匠等)** の入力形式（features1 = 62ch、features2 = 57ch）専用です。AobaZero 系モデル（features2 = 86ch）には対応しません。AobaZero モデルを与えると ONNX Runtime が input2 の次元不一致 (`Got: 57 Expected: 86`) で停止します。AobaZero 系は `rescore_psv` の `--onnx-model`（`aobazero-onnx` feature）を使ってください。

## 精度・比較の前提

- FP32 (CUDA EP) と FP16 (TensorRT EP) で評価値は微小に異なります。比較の再現性が要る場合は推論モードを固定してください。
- 詰み近傍の局面は DL の勝率飽和でクランプされた cp になるため、`eval_deep` との比較は比較側で除外する想定です（`eval_band == "mate"` 等で絞る）。

## メモリ

入力 jsonl は `--onnx-batch-size` 行ずつ streaming で読み、推論後に書き出します。全件を貯めないため**ピークメモリは入力件数に非依存**で、バッチ分の局面・JSON object と特徴量バッファの上限で頭打ちになります。
