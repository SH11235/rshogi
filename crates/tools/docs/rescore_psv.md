# rescore_psv — PSV 評価値の再スコアリング

PSV（PackedSfenValue）ファイルの評価値を ONNX モデルで再スコアリングするツール。
GPU 推論による高速処理に対応。

## 前提条件

- NVIDIA GPU + CUDA Toolkit（12.x 以上）
- ONNX Runtime 1.24.2 GPU 版
- cuDNN 9
- TensorRT 10（`--onnx-tensorrt` 使用時のみ、オプション）

## セットアップ

### 1. ONNX Runtime GPU 版

[ONNX Runtime Releases](https://github.com/microsoft/onnxruntime/releases) から
`onnxruntime-linux-x64-gpu-1.24.2.tgz`（Linux）または
`onnxruntime-win-x64-gpu-1.24.2.zip`（Windows）をダウンロード。

```bash
wget https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-linux-x64-gpu-1.24.2.tgz
tar xzf onnxruntime-linux-x64-gpu-1.24.2.tgz -C ~/lib/
```

> ort 2.0.0-rc.12（Release Candidate）は ONNX Runtime 1.24.2 向け。バージョンを合わせること。
> ort の安定版リリース後はバージョン対応表を要確認。

### 2. cuDNN 9

ONNX Runtime GPU 版は cuDNN 9 に依存する。

```bash
wget https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/linux-x86_64/cudnn-linux-x86_64-9.8.0.87_cuda12-archive.tar.xz
tar xf cudnn-linux-x86_64-9.8.0.87_cuda12-archive.tar.xz -C ~/lib/
```

### 3. TensorRT（オプション、`--onnx-tensorrt` 使用時のみ）

TensorRT EP を使うと FP16 推論により約 2.5 倍高速化される。

```bash
wget https://developer.nvidia.com/downloads/compute/machine-learning/tensorrt/10.11.0/tars/TensorRT-10.11.0.33.Linux.x86_64-gnu.cuda-12.9.tar.gz
tar xzf TensorRT-10.11.0.33.Linux.x86_64-gnu.cuda-12.9.tar.gz -C ~/lib/
```

> ORT 1.24.2 は `libnvinfer.so.10` を要求するため TensorRT 10.x が必要。

### 4. 環境変数

以下を `.bashrc` 等に追加する。

```bash
export ORT_DYLIB_PATH=~/lib/onnxruntime-linux-x64-gpu-1.24.2/lib/libonnxruntime.so
export LD_LIBRARY_PATH=~/lib/TensorRT-10.11.0.33/lib:~/lib/cudnn-linux-x86_64-9.8.0.87_cuda12-archive/lib:~/lib/onnxruntime-linux-x64-gpu-1.24.2/lib:/usr/local/cuda/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
```

TensorRT を使わない場合は `LD_LIBRARY_PATH` から TensorRT のパスを省略可。

| 環境変数 | 役割 |
|---|---|
| `ORT_DYLIB_PATH` | ONNX Runtime ライブラリ本体のパス（必須） |
| `LD_LIBRARY_PATH` | TensorRT・cuDNN・CUDA 等の依存ライブラリの検索パス |

**Windows の場合**: `LD_LIBRARY_PATH` の代わりにシステムの `PATH` を使う。

```powershell
$env:ORT_DYLIB_PATH = "C:\path\to\onnxruntime-win-x64-gpu-1.24.2\lib\onnxruntime.dll"
$env:PATH = "C:\path\to\TensorRT\lib;C:\path\to\onnxruntime-win-x64-gpu-1.24.2\lib;C:\path\to\cudnn\bin;" + $env:PATH
```

## 使い方

### ビルド

モデル形式に応じた feature フラグを指定する。

| feature | 対象モデル |
|---|---|
| `aobazero-onnx` | AobaZero 系 ONNX モデル |
| `dlshogi-onnx` | 標準 dlshogi 系 ONNX モデル（DL水匠等） |

```bash
cargo build --release -p tools --features aobazero-onnx --bin rescore_psv
# または
cargo build --release -p tools --features dlshogi-onnx --bin rescore_psv
```

### 実行例

```bash
# AobaZero ONNX モデル（GPU）
cargo run --release -p tools --features aobazero-onnx --bin rescore_psv -- \
  --input data/train.psv \
  --output-dir data/rescored/ \
  --onnx-model model.onnx \
  --onnx-batch-size 1024 \
  --onnx-gpu-id 0 \
  --onnx-eval-scale 600 \
  --threads 12

# 標準 dlshogi ONNX モデル（GPU）
cargo run --release -p tools --features dlshogi-onnx --bin rescore_psv -- \
  --input data/train.psv \
  --output-dir data/rescored/ \
  --dlshogi-onnx-model DL_suisho.onnx \
  --onnx-batch-size 1024 \
  --onnx-gpu-id 0 \
  --onnx-eval-scale 600 \
  --threads 12

# TensorRT + FP16（約 2.5 倍高速、初回はエンジンコンパイルに時間がかかる）
cargo run --release -p tools --features dlshogi-onnx --bin rescore_psv -- \
  --input data/train.psv \
  --output-dir data/rescored/ \
  --dlshogi-onnx-model DL_suisho.onnx \
  --onnx-batch-size 1024 \
  --onnx-gpu-id 0 \
  --onnx-tensorrt \
  --onnx-tensorrt-cache /tmp/trt_cache \
  --onnx-eval-scale 600 \
  --threads 12

# CPU 推論
cargo run --release -p tools --features aobazero-onnx --bin rescore_psv -- \
  --input data/train.psv \
  --output-dir data/rescored/ \
  --onnx-model model.onnx \
  --onnx-gpu-id=-1 \
  --threads 12
```

### 主要オプション

| オプション | デフォルト | 説明 |
|---|---|---|
| `--input` | （必須） | 入力 PSV ファイル（カンマ区切りで複数可） |
| `--output-dir` | （必須） | 出力ディレクトリ |
| `--onnx-model` | — | AobaZero ONNX モデルパス（`aobazero-onnx` feature 時） |
| `--dlshogi-onnx-model` | — | dlshogi ONNX モデルパス（`dlshogi-onnx` feature 時） |
| `--onnx-batch-size` | 256 | 推論バッチサイズ |
| `--onnx-gpu-id` | 0 | GPU ID（`-1` で CPU 推論） |
| `--onnx-tensorrt` | false | TensorRT EP を使用（FP16 推論） |
| `--onnx-tensorrt-cache` | — | TensorRT エンジンキャッシュの保存先 |
| `--onnx-eval-scale` | 600.0 | 勝率→cp 変換スケール |
| `--threads` | 1 | 処理スレッド数（rayon による特徴量構築の並列化） |

### `--threads` について

特徴量構築（CPU 処理）を rayon で並列化するスレッド数。
**本ツールのボトルネックは CPU→GPU のデータ転送（全処理時間の 96%、nsys 計測）であり、
CPU 側の特徴量構築を並列化しても全体時間は短縮されない**。デフォルトの `--threads 1` で問題ない。

### 計測例（DL_suisho.onnx, BS=1024, RTX 2070 Super）

| 構成 | --threads 4 | --threads 1 | 差 |
|---|---|---|---|
| CUDA FP32 (90,724 records) | 31.7 s | 33.4 s | 5%（計測ノイズ範囲） |
| TensorRT FP16 (90,724 records) | 11.6 s | 11.4 s | -2% |
| TensorRT FP16 (1,051,780 records, 温度管理付き) | 97.2 s | 96.9 s | 0.3% |

> 注: GPU のサーマルスロットリングが計測に大きく影響するため、
> 連続計測時は GPU 温度を 60℃ 以下に冷却してから実行すること。

## 動作確認

正常時の出力:

```
ORT_DYLIB_PATH: /home/user/lib/.../libonnxruntime.so
Loading AobaZero ONNX model: model.onnx
Using CUDA GPU 0
CUDA execution provider: available
AobaZero ONNX model loaded. Batch size: 1024
[00:00:05] ████████████████████ 6693/6693 (1234 rec/s) Processing...
```

## トラブルシューティング

| エラーメッセージ | 原因 | 対処 |
|---|---|---|
| `ORT_DYLIB_PATH environment variable is not set` | 環境変数未設定 | `ORT_DYLIB_PATH` に `libonnxruntime.so` のパスを設定 |
| `ORT_DYLIB_PATH is set to '...' but the file does not exist` | パスが間違っている | ファイルパスを確認 |
| `CUDAExecutionProvider is NOT available` | CPU 版ランタイムを使っている | GPU 版ランタイムをダウンロードして `ORT_DYLIB_PATH` を修正 |
| `libcudnn.so.9: cannot open shared object file` | cuDNN が見つからない | cuDNN 9 をインストールし `LD_LIBRARY_PATH` に追加 |
| `CUDA EP registration failed` | CUDA/cuDNN のバージョン不一致等 | CUDA Toolkit・cuDNN のバージョンを確認 |
| `TensorRTExecutionProvider is NOT available` | TensorRT が見つからない | `libnvinfer.so.10` を `LD_LIBRARY_PATH` に追加 |
| `--onnx-tensorrt requires a GPU` | TensorRT と CPU モードの併用 | `--onnx-gpu-id` を 0 以上に設定 |

## 技術的背景

本ツールは ONNX Runtime をバイナリに同梱せず、実行時に外部ライブラリとして読み込む。
このため `ORT_DYLIB_PATH` でライブラリの場所を明示的に指定する必要がある。

- `ORT_DYLIB_PATH` 未設定時はエラーを返す（未設定のまま実行するとハングするため）
- GPU モードでは起動時に CUDA が利用可能かチェックし、CPU への暗黙フォールバックを防止する
- `--onnx-tensorrt` で TensorRT ExecutionProvider (FP16) を使用可能
- TensorRT は常に FP16 で推論する。FP32 モード（`--onnx-tensorrt` なし）と比較して約 2.8 倍高速化されるが、
  評価値に平均 12cp 程度の差が出る（FP16 の方が系統的にやや高く出る傾向）
- TensorRT FP32 は計測の結果 CUDA EP より遅いため（カーネル最適化の効果よりセッション初期化コストが大きい）、
  FP32 で推論する場合は `--onnx-tensorrt` を指定せず CUDA EP を使うこと
- TensorRT は初回実行時にモデルを GPU 固有にコンパイルする（数十秒〜数分）。
  `--onnx-tensorrt-cache` でキャッシュを保存すると 2 回目以降は高速起動する
- このツールのボトルネックは CPU→GPU のデータ転送（全処理時間の 96%、nsys 計測）であり、
  FP16 による高速化は主に転送量の半減と Tensor Core 活用に起因する
- `--threads` による特徴量構築の並列化は、ボトルネックが CPU→GPU 転送（96%）に
  あるため原理的に全体時間への影響がない。計測でもいずれの構成 (CUDA FP32 / TensorRT FP16、
  90k / 1.05M records) で有意な差は観測されなかった
- 参考: 同等の Python ツール [psv-utils](https://github.com/KazApps/psv-utils) と比較して、
  本ツールは CUDA EP / TensorRT EP どちらでも約 6〜9% 速い（1,051,780 records, 温度管理付き計測）
