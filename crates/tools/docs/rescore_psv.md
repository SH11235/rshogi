# rescore_psv — PSV 評価値の再スコアリング + ポリシー展開

PSV（PackedSfenValue）ファイルの評価値を ONNX モデルで再スコアリングするツール。
GPU 推論による高速処理に対応。

`--expand-output-dir` を指定すると、**同一の ONNX 推論結果から value と policy を
両方取り出して、rescore と局面展開を 1 パスで実行** できる（`expand_psv_from_policy`
相当の処理を統合）。

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

# rescore + ポリシー展開を 1 パスで実行（--expand-output-dir）
# 同一推論で value → rescore 出力、policy → 子局面出力
cargo run --release -p tools --features dlshogi-onnx --bin rescore_psv -- \
  --input data/train.psv \
  --output-dir data/rescored/ \
  --expand-output-dir data/expanded/ \
  --expand-threshold 10.0 \
  --dlshogi-onnx-model DL_suisho.onnx \
  --onnx-batch-size 1024 \
  --onnx-gpu-id 0 \
  --onnx-tensorrt \
  --onnx-tensorrt-cache /tmp/trt_cache \
  --onnx-eval-scale 600
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
| `--onnx-eval-scale` | 600.0 | 勝率→cp 変換スケール（有限値・正値必須） |
| `--skip-in-check` | false | 王手親局面の rescore 出力を抑制（後述） |
| `--threads` | 1 | 処理スレッド数（rayon による特徴量構築の並列化） |

### ポリシー展開オプション（`--expand-output-dir` 指定時）

| オプション | デフォルト | 説明 |
|---|---|---|
| `--expand-output-dir` | — | 展開された子局面の出力ディレクトリ。**指定時のみ expand 有効。ONNX モード必須** |
| `--expand-threshold` | 10.0 | 合法手 softmax 確率がこの値（%）を超えた手を子局面として出力。`(0.0, 100.0]` の有限値 |
| `--expand-skip-parent-in-check` | false | 親が王手なら expand をスキップ（`--skip-in-check` と独立） |
| `--expand-skip-child-in-check` | false | 展開した子局面が王手なら expand 出力をスキップ |

出力ファイル名は入力ファイル名と同じ。`--output-dir` と `--expand-output-dir` は
別ディレクトリを指定する必要がある（同一指定は起動時エラー）。

### `--skip-in-check` の挙動

王手局面を rescore 出力から除外するフラグ。**ONNX モードでは 2026-04 以降、
挙動が変更されている**:

- 旧挙動: 推論バッチに入れる前にドロップ（推論もスキップ）
- 新挙動: **推論は実行し、rescore の書き出しだけ抑制**（expand 機能と独立動作させるため）

出力 PSV のバイト列は変わらない。推論コストは王手親局面の割合分わずかに増加する
（教師データ中の王手局面は通常 1 桁 %）。
`--expand-skip-parent-in-check` と `--expand-skip-child-in-check` で expand 側の
王手フィルタを独立に制御できる。

### ポリシー展開（`--expand-output-dir`）について

`--expand-output-dir` を指定すると、同じ ONNX 推論の policy 出力を使って
合法手の softmax 確率を計算し、閾値を超えた手の子局面を PSV として書き出す。
`expand_psv_from_policy` を別パスで走らせるのと同等の結果を、**推論 1 パス**で
得られる。

子局面 PSV の `score` / `move16` / `game_result` は 0 で初期化される。子局面に
スコアを付与したい場合は、出力した expand 結果を改めて `rescore_psv` に通す
（そのときは `--expand-output-dir` なしで value rescore のみ）。

### 完了マーカー（`<rescore_output>.done`）

ONNX モードでは、各入力ファイルの処理完了時に rescore 出力の隣に
`<ファイル名>.done` という sidecar テキストを atomic rename で書き出す。
次回同じ入力に対して実行すると:

- **marker の設定 fingerprint が現在の CLI と完全一致 + 出力サイズが記録と一致** → ファイル skip
- **fingerprint 不一致（モデル差し替え・`--onnx-eval-scale` 変更・expand 設定変更など）** →
  rescore/expand 両出力を truncate して再生成
- **marker が無い（従来互換）+ expand 無効** → 既存のレコード数ベース resume にフォールバック
- **marker が無い + expand 有効** → 両出力を truncate して最初から処理

fingerprint に含まれる項目:

- モデルパス（canonicalize 済み）、モデルサイズ、モデル mtime（ns）
- 入力パス、入力サイズ、入力 mtime（ns）
- `process_count`（`--limit` 適用後）
- `--skip-in-check`、`--score-clip`、`--onnx-eval-scale`（`f32::to_bits()` の hex で保存）
- AobaZero モデル時のみ `--onnx-draw-ply`
- expand 有効時: `--expand-threshold`（to_bits hex）、`--expand-skip-parent-in-check`、
  `--expand-skip-child-in-check`、`--expand-output-dir` の canonicalize 済みパス

`Ctrl-C` で中断した場合は marker を書き出さない（中途半端に処理したファイルを
完了扱いにしない）。プロセス kill / panic には atomic rename + `sync_all()` で
対応。電源断・カーネルパニックは非目標。

#### パスに使える文字の制約

マーカーは単純な `key=value\n` テキスト形式で保存される。round-trip を保証する
ため、**model / input / expand_output_dir のパスに以下の文字を含めると起動時
エラーで弾かれる**:

- `=` (key/value セパレータと衝突、例: `v1.0=alpha/model.onnx`)
- `\n` / `\r` (レコードセパレータと衝突)
- 非 UTF-8 バイト列（Windows では実質発生しない、Linux の古い / 非 UTF-8 FS 由来）

エラーが出た場合はパスをリネームして回避する。これらの文字を path に含める
ユースケースは稀なので通常は気にする必要はない。

### パス安全チェック（ONNX モード）

ONNX モードでは以下を起動時 / ファイルごとに検証し、データ破壊を防止する:

- `--output-dir` と `--expand-output-dir` が同一ディレクトリ → エラー
- 入力ファイル = 予定出力パス（未作成でも parent canonicalize で検出）→ エラー
- 既存出力が symlink → エラー（symlink 越しの truncate で入力を破壊しないため）
- Unix のみ: 既存出力が入力と同じ inode（hardlink）→ エラー
- marker 不一致で旧 expand artifact を削除する前に、旧 artifact が現在の入力と
  同一実体でないことを検証 → 同一なら削除せずエラー（段階的パイプライン対策）

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

## ユースケース別の使い分け

DL 系 ONNX モード（`--dlshogi-onnx-model` / `--onnx-model`）での代表的な
運用パターン。各フラグは独立に組み合わせ可能。

### 1. 王手局面を教師データから除外する

王手親局面は評価が不安定になりやすい（詰み・詰めろ・王手放置などが混在）ため、
学習ノイズを減らしたい場合に使う:

```bash
rescore_psv --input "data/*.bin" --output-dir rescored/ \
  --dlshogi-onnx-model model.onnx \
  --skip-in-check
```

rescore 出力から王手親レコードが除外される（推論は実行され、書き出しだけ抑制）。
出力サイズは「入力 - 王手局面数」。

### 2. 親と子の王手フィルタを別々に制御する（expand 併用）

expand 側と rescore 側で独立に王手除外を制御したい場合:

```bash
# 例 A: rescore には王手親も含める、expand 側は王手親から展開しない
rescore_psv --input data.bin --output-dir rescored/ \
  --expand-output-dir expanded/ \
  --dlshogi-onnx-model model.onnx \
  --expand-skip-parent-in-check

# 例 B: 展開した子局面が王手状態になるものを除外（"王手に追い込んだ手" を学習対象から外す）
rescore_psv --input data.bin --output-dir rescored/ \
  --expand-output-dir expanded/ \
  --dlshogi-onnx-model model.onnx \
  --expand-skip-child-in-check
```

`--skip-in-check` / `--expand-skip-parent-in-check` / `--expand-skip-child-in-check`
の 3 フラグは独立。全部同時 ON で「王手が関係する局面を全排除」にもできる。

### 3. 大規模 shard を段階的に処理する（glob + レジューム）

数十〜数百 shard を逐次処理し、中断・設定変更があっても再開できる:

```bash
rescore_psv --input "data/shard_*.bin" --output-dir rescored/ \
  --expand-output-dir expanded/ \
  --dlshogi-onnx-model model.onnx \
  --onnx-tensorrt --onnx-tensorrt-cache /tmp/trt_cache \
  --expand-threshold 10.0
```

各 shard 完了時に `rescored/shard_XXX.bin.done` マーカーが書き出される。
次回実行時の挙動:

- 設定変更なし + 全 shard 完了済み → 全 skip
- 一部 shard のみ完了 → 未完了 shard だけ処理（完了分はマーカーで skip）
- `--onnx-eval-scale` やモデルを変更 → marker 不一致で対象 shard を自動再生成

`nohup` / `tmux` で流しっぱなしにしておき、GPU 温度で止めた後の再開や、
モデル差し替え時の一括再スコアに使える。

### 4. 段階的パイプライン（多段 expand + rescore）

policy で得た子局面をさらに展開、のようにカバレッジを段階的に広げる:

```bash
# ステップ 1: 元データを rescore + 1 次展開
rescore_psv --input "data/*.bin" --output-dir rescored/ \
  --expand-output-dir expanded1/ \
  --dlshogi-onnx-model model.onnx

# ステップ 2: 1 次展開の子局面を入力にして、さらに rescore + 2 次展開
rescore_psv --input "expanded1/*.bin" --output-dir rescored_expanded1/ \
  --expand-output-dir expanded2/ \
  --dlshogi-onnx-model model.onnx
```

**運用上の注意**:

- 各段で `--output-dir` と `--expand-output-dir` は **別ディレクトリ**を指定
- 入力ディレクトリ（例 `expanded1/`）と新しい `--expand-output-dir`（例
  `expanded2/`）も **別ディレクトリ**を指定
- 旧段の expand 出力が次段の入力と同じファイル実体になる誤設定は起動時に検出
  してエラー（安全装置、PR #463 で追加）

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
| `--expand-output-dir requires ONNX mode` | NNUE/USI モードで expand 指定 | ONNX モード（`--onnx-model` / `--dlshogi-onnx-model`）を使う |
| `--expand-threshold must be a finite value in (0.0, 100.0]` | 範囲外 / NaN / inf | 有限値かつ `0 < v <= 100` を指定 |
| `--onnx-eval-scale must be a positive finite value` | 0 以下 / NaN / inf | 正の有限値（通常 600.0）を指定 |
| `--output-dir and --expand-output-dir must point to different directories` | 同一ディレクトリ指定 | 別ディレクトリを指定 |
| `Output path is a symlink (refusing to truncate a symlink)` | 出力予定パスが symlink | symlink を削除するか別ディレクトリを使う |
| `Output path is a hardlink to the input file` | 出力予定が入力の hardlink（Unix） | 別ディレクトリを指定 |
| `Stale expand artifact ... resolves to the current input file` | 旧 expand 出力と現在 input が同一（段階的パイプライン） | 入力を移動するか `--expand-output-dir` を変更 |
| `... path contains '=' which is not supported by the completion marker` | モデル/入力/expand 出力パスに `=` が含まれる | パスをリネーム（`v1.0=alpha` → `v1.0-alpha` など） |
| `... path contains non-UTF-8 characters` | パスに非 UTF-8 バイト列 | パスを UTF-8 に揃える |

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
