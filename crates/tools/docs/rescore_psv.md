# rescore_psv — PSV 評価値の再スコアリング + ポリシー展開

PSV（PackedSfenValue）ファイルの評価値（score）を付け替えるツール。全モードが
ストリーミングで動作し、**ピークメモリは入力件数に依存しない**ため、数十億局面の
ファイルもそのまま処理できる。

| モード | 評価器 | 用途 |
|---|---|---|
| **ONNX 直接推論（推奨）** | dlshogi 系 / AobaZero 系 ONNX モデル | GPU による大量リスコア。TensorRT FP16 で最速 |
| 内部 NNUE（静的 / qsearch） | `--nnue` で指定した NNUE | CPU のみで完結する軽量リスコア |
| 内部 NNUE + 本探索 | `--search-depth` | 探索スコアでのラベル付け |
| 外部 USI エンジン | `--engine` | rshogi に載らない評価器（DL 系 USI エンジン等） |

主要ユースケースは **dlshogi 系 ONNX モデル + TensorRT FP16** での一括リスコア。
以下のクイックスタートがその導線になっている。

## クイックスタート（推奨: dlshogi ONNX + TensorRT FP16）

### 1. 環境変数

初回はライブラリの導入が必要（後述「セットアップ」参照）。導入済みなら以下の
2 変数が通っていることを確認する:

```bash
export ORT_DYLIB_PATH=~/lib/onnxruntime-linux-x64-gpu-1.24.2/lib/libonnxruntime.so
export LD_LIBRARY_PATH=~/lib/TensorRT-10.11.0.33/lib:~/lib/cudnn-linux-x86_64-9.8.0.87_cuda12-archive/lib:~/lib/onnxruntime-linux-x64-gpu-1.24.2/lib:/usr/local/cuda/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
```

### 2. 実行

```bash
cargo build --release -p tools --bin rescore_psv   # default build でそのまま使える

target/release/rescore_psv \
  --input "data/shard_*.bin" \
  --output-dir rescored/ \
  --dlshogi-onnx-model DL_suisho.onnx \
  --onnx-tensorrt \
  --onnx-tensorrt-cache trt_cache/ \
  --onnx-eval-scale 600
```

- `--onnx-tensorrt`: FP16 推論で FP32（CUDA EP）比 約 2.5〜2.8 倍高速。評価値は
  FP32 比で平均 12cp 程度ずれる（やや高めに出る傾向）。厳密に FP32 が必要な場合
  のみ外す（そのとき `--onnx-tensorrt` なしの CUDA EP が最速。TensorRT の FP32
  モードは CUDA EP より遅い）。
- `--onnx-tensorrt-cache`: **実質必須**。初回にその GPU 向けエンジンをビルドして
  保存し（数十秒〜数分）、2 回目以降は再利用する。未指定だと毎回ビルドし直す。
  キャッシュは GPU アーキ固有で、GPU を変えたら自動で再ビルドされる。
- `--onnx-eval-scale 600`: 勝率→cp 変換スケール。dlshogi 系の標準値。
- 長時間ジョブは `nohup` / `tmux` で流しっぱなしにする（非 TTY では自動で
  grep しやすいログ行形式になる）。

### 3. 進捗の読み方

```
全体 38%  3.84M/10.0M  (shard 2/4)  8.3k/s  残り 12:40  完了 06/22 14:43
└ shard_003  52% ███████████░░░░░░░░░  523.5k/1.00M
```

`完了`（ログ行では `ETA`）は残り時間ではなく完了予定の**時刻**（`MM/DD HH:MM`）。
入力に破損レコード等があるとその分 100% に届かないことがあるが、rescore 出力
（有効レコードのみ）には影響しない（終了時の `Note:` 行で件数を確認できる）。

### 4. 中断と再開

- shard ごとに完了マーカー `rescored/<入力名>.done` が書かれる。**中断したら同じ
  コマンドを再実行するだけ**で、完了済み shard は skip され、未完了 shard から
  再開する。
- モデルや `--onnx-eval-scale` 等の設定を変えて再実行すると、マーカー不一致を
  検知して該当 shard を自動で最初から再生成する。
- `Ctrl-C` での中断は安全（処理途中のファイルを完了扱いにしない。部分出力は
  入力の連続 prefix になる）。

詳細は「再開（resume）の仕組み」参照。

## セットアップ（初回のみ）

前提: NVIDIA GPU + CUDA Toolkit 12.x（本手順の ONNX Runtime / cuDNN / TensorRT は
いずれも CUDA 12 系ビルド）、ONNX Runtime 1.24.2 GPU 版、cuDNN 9、TensorRT 10
（`--onnx-tensorrt` 使用時のみ）。バージョンは揃えること:

| コンポーネント | バージョン | 備考 |
|---|---|---|
| ONNX Runtime GPU | 1.24.2 | ort crate 2.0.0-rc.12 対応版。CUDA 12 ビルドを使う |
| cuDNN | 9.x (9.8.0.87) | ORT GPU 版の依存 |
| TensorRT | 10.x (10.11.0.33) | ORT 1.24.2 は `libnvinfer.so.10` を要求 |

> ort crate は 2.0.0-rc.12（Release Candidate）。ort の安定版リリース後は
> バージョン対応表を要確認。

```bash
wget https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-linux-x64-gpu-1.24.2.tgz
tar xzf onnxruntime-linux-x64-gpu-1.24.2.tgz -C ~/lib/
wget https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/linux-x86_64/cudnn-linux-x86_64-9.8.0.87_cuda12-archive.tar.xz
tar xf cudnn-linux-x86_64-9.8.0.87_cuda12-archive.tar.xz -C ~/lib/
wget https://developer.nvidia.com/downloads/compute/machine-learning/tensorrt/10.11.0/tars/TensorRT-10.11.0.33.Linux.x86_64-gnu.cuda-12.9.tar.gz
tar xzf TensorRT-10.11.0.33.Linux.x86_64-gnu.cuda-12.9.tar.gz -C ~/lib/
```

環境変数（クイックスタート参照）を `.bashrc` 等に追加する。`ORT_DYLIB_PATH` は
ONNX Runtime を実行時に dlopen するために必須（未設定はエラーになる）。
`LD_LIBRARY_PATH` は TensorRT・cuDNN・CUDA 等の依存ライブラリの検索パスで、
TensorRT を使わない場合は TensorRT のパスを省略できる。GPU モードでは起動時に
CUDA が利用可能かチェックし、CPU への暗黙フォールバックを防止する。

### Windows / 特定 GPU の補足

- Windows は同一バージョンの Windows 版を導入し、`LD_LIBRARY_PATH` の代わりに
  `PATH` へ追加する（`$env:ORT_DYLIB_PATH = "C:\path\to\onnxruntime.dll"` /
  `$env:PATH = "C:\path\to\TensorRT\lib;...;" + $env:PATH`）。直リンク:
  [ONNX Runtime](https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-win-x64-gpu-1.24.2.zip)（CUDA 12 ビルド。`cuda13` 版ではない）/
  [cuDNN 9](https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/windows-x86_64/cudnn-windows-x86_64-9.8.0.87_cuda12-archive.zip)/
  [TensorRT 10.11](https://developer.nvidia.com/downloads/compute/machine-learning/tensorrt/10.11.0/zip/TensorRT-10.11.0.33.Windows.win10.cuda-12.9.zip)
- RTX 5090 / Blackwell (sm_120) はこの構成でそのまま動作する（キャッシュ生成物例:
  `<cache dir>/TensorrtExecutionProvider_..._fp16_sm120.engine`）。
- 外部データ形式の ONNX（本体 〜150KB + `.onnx.data` 数十 MB のモデル）は両ファイル
  を同一ディレクトリに置く（ORT が本体からの相対パスで外部ウェイトを読むため）。

## ビルド

dlshogi 系 ONNX（`--dlshogi-onnx-model`）は default feature。AobaZero 系
（`--onnx-model`）を使う場合のみ `aobazero-onnx` を追加する。

| feature | 対象モデル | default |
|---|---|---|
| `dlshogi-onnx` | 標準 dlshogi 系 ONNX（DL水匠等、features2=57ch） | ✅ |
| `aobazero-onnx` | AobaZero 系 ONNX（カスタム特徴量） | — |

```bash
cargo build --release -p tools --bin rescore_psv
cargo build --release -p tools --features aobazero-onnx --bin rescore_psv  # AobaZero も使う場合
```

ort は load-dynamic のためビルド時に libonnxruntime は不要（実行時に dlopen する）。

## オプションリファレンス

### 共通

| オプション | デフォルト | 説明 |
|---|---|---|
| `--input` | （必須） | 入力 PSV。glob / 複数指定可 |
| `--output-dir` | （必須） | 出力ディレクトリ（入力ファイル名で出力） |
| `--out-scores` | false | dlshogi ONNX の score のみを `<入力名>.scores.i16` に出力（後述） |
| `--threads` | 0（論理コア数） | 並列処理スレッド数 |
| `--limit` | 0（無制限） | 各入力ファイルで処理するレコード数上限（`--out-scores` と併用不可） |
| `--score-clip` | 10000 | スコアを ± この値にクリップ |
| `--skip-in-check` | false | 王手局面を出力から除外 |
| `--delete-input` | false | 各ファイル処理完了後に入力を削除（ディスク節約、`--out-scores` と併用不可） |
| `--verbose` | false | 詳細出力 |

### ONNX モード

| オプション | デフォルト | 説明 |
|---|---|---|
| `--dlshogi-onnx-model` | — | dlshogi 系 ONNX モデルパス |
| `--onnx-model` | — | AobaZero 系 ONNX モデルパス（`aobazero-onnx` feature） |
| `--onnx-batch-size` | 1024 | 推論バッチサイズ（チューニング指針参照） |
| `--onnx-gpu-id` | 0 | GPU 番号（複数 GPU 時の選択。`-1` で CPU 推論） |
| `--onnx-sessions` | 2 | GPU 推論の多重化数（1〜4、CPU 推論では常に 1）。VRAM は増える（チューニング指針参照） |
| `--onnx-tensorrt` | false | TensorRT EP（FP16）を使用 |
| `--onnx-tensorrt-cache` | — | TensorRT エンジンキャッシュ保存先（実質必須） |
| `--onnx-eval-scale` | 600.0 | 勝率→cp 変換スケール（正の有限値） |
| `--onnx-draw-ply` | 0（調整なし） | AobaZero モデル用の引き分け手数 |
| `--expand-output-dir` | — | policy 展開の出力先（レシピ参照） |
| `--expand-threshold` | 10.0 | 展開する softmax 確率閾値 %（`(0, 100]`） |
| `--expand-skip-parent-in-check` | false | 親局面が王手なら expand をスキップ（rescore 側は `--skip-in-check` で別制御） |
| `--expand-skip-child-in-check` | false | 展開した子局面が王手なら expand 出力をスキップ |
| `--ort-profile` | — | ORT profiling 出力先ディレクトリ（`session.run()` の内訳を JSON 出力、開発用） |
| `--qsearch-leaf-label` | false | ラベルのみ葉評価にする（`--nnue` 併用必須、レシピ参照） |
| `--qsearch-leaf-replacement-output` | — | 葉置換 arm の同時出力先（レシピ参照） |

### 内部 NNUE / 探索 / 外部エンジンモード

| オプション | デフォルト | 説明 |
|---|---|---|
| `--nnue` | — | NNUE モデル（ONNX / `--engine` 未使用時に必須） |
| `--use-qsearch` | false | 静的評価の代わりに qsearch 評価を使用 |
| `--search-depth` | — | 指定深さの alpha-beta 探索スコアを使用（`--use-qsearch` と排他） |
| `--emit-bestmove` | false | `--search-depth` の探索結果の最善手を実 YaneuraOu PSV 形式 (A) で move16 に出力 |
| `--hash-mb` | 64 | スレッドごとの置換表サイズ MB（`--search-depth` 時） |
| `--max-nodes` / `--max-time` | 0（無制限） | 1 局面あたりの探索ノード / ミリ秒上限（`--search-depth` 時のみ有効）。探索爆発ガード |
| `--max-ply` | 16 | qsearch の最大深さ |
| `--apply-qsearch-leaf` | false | 局面を qsearch 葉に置換して出力 |
| `--source-fv-scale` / `--target-fv-scale` | 24 / 24 | FV_SCALE 変換（通常は変換不要） |
| `--engine` | — | 外部 USI エンジンパス。内部 NNUE の代わりに評価 |
| `--engine-nodes` | 1 | エンジンの `go nodes` 値（0 で `go depth 1`） |
| `--engine-threads` | 1 | 並列エンジンプロセス数（DL 系は VRAM に応じ 2〜4） |
| `--usi-option` | — | `Name=Value` 形式、複数可（例: `DNN_Model=model.onnx`） |
| `--engine-timeout` | 600 | エンジン応答タイムアウト秒（TensorRT 初回ビルド対策で長め） |

### チューニング指針

- `--onnx-batch-size`: GPU 推論では既定 1024 のままでよい。実測（RTX 3080 Ti /
  RTX 5090、DL水匠 + TensorRT FP16、1M 局面）では 512〜2048 はスループットが
  ほぼフラットで、256 以下だけ明確に遅い（3080 Ti で −3%、5090 で −10〜15%。
  `--onnx-sessions 1` だと差はさらに開く）。「VRAM の許す限り大きく」しても
  単調には速くならず、メモリを余分に食うだけの領域がある（参考: 3080 Ti /
  sessions=2 の VRAM peak は batch 1024 で約 1.5GiB、2048 で約 1.7GiB）。
  最適点は GPU / モデル / EP でずれるため、大規模 rescore の前に数十万局面の
  サンプルで 512/1024/2048 を振って pos/s を比べるのが確実。この計測条件では
  出力がバッチサイズ / セッション数に依らず sha256 一致することを確認済み
  （一般保証ではないので、厳密な再現性が要る場合は同一設定で流し切る）。
  ホスト RAM 側も特徴量バッファがバッチサイズ × セッション数に比例して増える。
  バッファは 2 × sessions slot 確保され、1 slot = batch × 1 局面の特徴量サイズ
  （dlshogi 系は 9,639 f32 ≈ 38 KiB）なので、既定 1024 × sessions 2 では概算
  約 150 MiB。CPU 推論（`--onnx-gpu-id -1`）や省メモリ環境では 256/512 も
  含めて計測して選ぶ。
- `--threads`: 特徴量構築（CPU 前処理）の並列数。GPU 推論とオーバーラップされる
  ため、重いモデルではデフォルトで足りる。軽量モデル + 高速 GPU で前処理が律速
  になる場合のみ増やす。
- `--onnx-sessions`: 上記実測（2 GPU / DL水匠）では既定 2 が最適（sessions=1
  比 +13〜36% @3080 Ti、+3〜10% @5090）。多重化がバッチ間の GPU アイドルを
  埋めるため、バッチサイズへの感度も下がる。GPU が電力上限に達している環境
  では 3 以上に増やしても伸びない。

## ユースケース別レシピ

### 大規模 shard の段階処理（glob + レジューム）

数十〜数百 shard を逐次処理し、中断・設定変更があっても再開できる。クイック
スタートのコマンドがそのままこの形（`--input "data/shard_*.bin"`）。
`nohup` / `tmux` で流しっぱなしにしておき、GPU 温度で止めた後の再開や、モデル
差し替え時の一括再スコアに使える。

### score sidecar 出力（`--out-scores`）

```bash
rescore_psv --input "data/*.psv" --output-dir scores/ \
  --dlshogi-onnx-model model.onnx --out-scores
```

フル PSV の代わりに `<入力ファイル名>.scores.i16` を生成する。record `i` の score は
offset `i * 2` にある little-endian i16 で、ファイルサイズは厳密に
`base PSV の全 records × 2` byte となる。通常のフル PSV
出力と同じ推論・符号変換・clip を通り、
通常出力の offset 32..34 を行順に抽出したものと bit 一致する。

このモードは `--dlshogi-onnx-model` の直接推論専用で、NNUE、qsearch、search、外部
engine、AobaZero ONNX、leaf label/replacement、expand、入力を削除する `--delete-input`、
処理件数を制限する `--limit`（0 より大きい値）、および行を減らす `--skip-in-check` とは
併用できない。入力は 40 byte の倍数でなければ
即時エラーになる。run 開始時に `<入力名>.scores.i16.in-progress` を作り、
入力・ONNX モデル・ラベル条件の fingerprint を記録する。中断後はこの marker が現在の
設定と一致し、既存 sidecar が 2 byte の倍数であるときだけ、サイズを 2 で割った record
位置から追記再開する。一致時に奇数 byte なら即時エラーになる。marker が無い、壊れている、
または fingerprint が異なる場合は、既存サイズにかかわらず sidecar を truncate して最初から生成する。
完了時は全 record 数との厳密一致を再検証して `.done` marker を書き、in-progress marker を
削除する。decode / 局面構築エラーを含むバッチは書き出し前に停止するため、既存 sidecar の
prefix は常に base PSV と行対応する。

### 王手局面を教師データから除外する

```bash
rescore_psv --input "data/*.bin" --output-dir rescored/ \
  --dlshogi-onnx-model model.onnx --skip-in-check
```

王手親局面は評価が不安定になりやすい（詰み・詰めろ・王手放置が混在）ため、
学習ノイズを減らしたい場合に使う。ONNX モードでは推論自体は実行し書き出しだけを
抑制するので、expand 機能とは独立に働く（推論コストは王手親局面の割合分だけ僅かに
増える。教師データ中の王手局面は通常 1 桁 %）。

### ポリシー展開（`--expand-output-dir`）

同一の ONNX 推論から value と policy を両方取り出し、rescore と子局面展開を
1 パスで実行する:

```bash
rescore_psv --input data.bin --output-dir rescored/ \
  --expand-output-dir expanded/ \
  --dlshogi-onnx-model model.onnx \
  --expand-threshold 10.0
```

- 合法手の softmax 確率が `--expand-threshold`（%、`(0, 100]`）を超えた手の
  子局面を PSV として書き出す。子局面の `score` / `move16` / `game_result` は 0
  初期化されるので、スコアが必要なら展開結果を改めて `rescore_psv` に通す。
- `--expand-skip-parent-in-check` / `--expand-skip-child-in-check` で expand 側の
  王手フィルタを `--skip-in-check`（rescore 側）と独立に制御できる。
- 出力ファイル名は入力ファイル名と同じ。`--output-dir` と `--expand-output-dir` は
  別ディレクトリ必須（起動時エラー）。
- 多段パイプライン（展開結果をさらに展開）も可能。各段で入力・出力・expand 先を
  すべて別ディレクトリにする。誤設定（旧段の出力 = 次段の入力の同一実体など）は
  起動時に検出してエラーになる。

### 葉ラベル / 葉置換（`--qsearch-leaf-label`）

DL 系の静的評価でラベル付けする際、PV 末端（静かな局面）の評価を教師ラベルに
したい場合に使う。局面は root のまま保持し、ラベルだけを qsearch 葉の ONNX 評価
にする:

```bash
rescore_psv --input "data/*.bin" --output-dir rescored_leaflabel/ \
  --dlshogi-onnx-model model.onnx \
  --nnue suisho5.bin \
  --qsearch-leaf-label
```

- `--nnue`（葉探索用）と `--dlshogi-onnx-model`（葉ラベル用）の両方が必須。
  dlshogi モデル専用（AobaZero は特徴量が手数 game_ply を含み、葉へ進めても root の
  game_ply が混入するため非対応）。`--apply-qsearch-leaf` / expand と併用不可。
- 葉で手番が反転した場合は root 手番視点へ符号反転する。王手 root は葉探索せず
  原局面のまま評価する。
- `--qsearch-leaf-replacement-output <dir>` を併用すると、同一 1 パスで
  **葉局面に置換した**レコード（leaf-REPLACEMENT arm）も別ディレクトリに書き出せる。
  2 工程（`--apply-qsearch-leaf` → DL rescore）と bit 一致し、再計算を半減できる。
  `--output-dir` とは別ディレクトリ必須。レコード仕様の詳細は
  [internals](rescore_psv-internals.md) 参照。

### 内部 NNUE / 探索 / 外部エンジンでリスコアする

```bash
# 静的 NNUE 評価（CPU のみ、最軽量）
rescore_psv --input data.bin --output-dir rescored/ --nnue nn.bin

# qsearch 評価
rescore_psv --input data.bin --output-dir rescored/ --nnue nn.bin --use-qsearch

# depth 指定探索（探索スコアでラベル付け）
rescore_psv --input data.bin --output-dir rescored/ --nnue nn.bin \
  --search-depth 8 --max-nodes 1000000 --threads 16

# 外部 USI エンジン（DL 系エンジン等）
rescore_psv --input data.bin --output-dir rescored/ \
  --engine /path/to/usi_engine --engine-nodes 100000 --engine-threads 2 \
  --usi-option "DNN_Model=model.onnx"
```

`--search-depth` の主なメモリ消費は置換表（`--hash-mb` × スレッド数）で、合計
4 GB を超えると起動時に警告が出る。外部エンジンはプロセスが死んだ場合、担当分の
未評価レコードを生存エンジンに再割り当てして継続する（全滅時はエラー終了し、
`--delete-input` でも入力を保全する）。

## 再開（resume）の仕組み

- **ONNX モード**: 入力ファイルごとに完了マーカー `<出力名>.done` を書く。
  再実行時、マーカーの設定 fingerprint（モデル・入力・主要フラグ）が現在の CLI と
  一致し出力サイズも一致すれば skip、不一致なら該当ファイルの全出力を truncate
  して自動再生成する。`Ctrl-C` 中断時は `.done` を書かない。
- **`--out-scores` の中断再開**: `<出力名>.in-progress` に `.done` と同じ形式の
  fingerprint を保持する。入力 path/size/mtime、モデル path/size/mtime、全入力件数、
  `--onnx-eval-scale`、`--score-clip`、`--onnx-tensorrt` など fingerprint が追跡する
  条件が一致する場合だけ既存 prefix へ追記する。不一致・marker 欠落時は truncate して
  再生成する。run 中に入力またはモデルの metadata が変化した場合も `.done` を書かず、
  次回実行時に再生成する。出力 sidecar
  自身の mtime は fingerprint に含めないため、追記による mtime 更新は resume を妨げない。
  `Ctrl-C` 中断時は in-progress marker を残し、正常完了時だけ `.done` へ昇格する。
  同じ出力 sidecar への複数プロセスからの同時書き込みは非対応。
- **NNUE / 探索 / 外部エンジンモード**: マーカーは使わず、出力レコード数が入力
  レコード数以上のファイルを skip する（ファイル粒度。中途半端なファイルは最初
  から再処理）。
- fingerprint の全項目やマーカーのパス文字制約（`=` / 改行 / 非 UTF-8 を含む
  パスは起動時エラー）は [internals](rescore_psv-internals.md) 参照。

## メモリと決定性

- 全モードでピークメモリは入力件数に非依存（NNUE / 探索 / 外部エンジンは
  100 万件チャンク、ONNX はバッチ単位の固定 slot パイプライン）。
- **ONNX モード**: 出力は `--onnx-sessions` / `--threads` に依存せず bit 一致。
  TensorRT は同一エンジンキャッシュ（warm）での実行が bit 再現の基準（cold の
  初回ビルドでは最下位 bit が変わりうる）。
- **`--search-depth` / `--engine`**: 同一設定での再実行は bit 一致するが、
  スレッド数 / エンジンプロセス数を変えると局面割り当てが変わり出力スコアも
  変わる（置換表・エンジン内部状態が担当局面列を通して持ち越されるため）。
  再現性が必要ならスレッド数（`--threads`）/ エンジンプロセス数（`--engine-threads`）
  を明示的に固定し、`--max-time` は使わないこと。`--engine` では外部エンジン自体が
  決定的であること（内部スレッド数 1 等）も条件。

## 終了動作（ONNX モード）

ONNX モードの正常終了は、全出力（rescore 出力・`.done` マーカー）を flush した後、
通常のプロセス teardown を踏まずに即時終了する（`_exit`）。ONNX Runtime
（TensorRT EP）をロードしたプロセスは exit 時のライブラリデストラクタ連鎖が
glibc ヒープを壊し、全処理の正常完了後に `corrupted double-linked list` abort
（exit code 134）を間欠的に起こすため、その回避。出力は即時終了前に書き終わって
おり欠けない。エラー終了は従来経路のため稀に exit 134 で落ちることがあるが、
その場合も stderr のエラーメッセージが一次情報。

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
| `Got invalid dimensions for input: input2` 等 | モデルの特徴量形式が不一致 | dlshogi 標準（57ch）は `--dlshogi-onnx-model`、AobaZero は `--onnx-model` を使う |
| `--expand-output-dir requires ONNX mode` | NNUE/USI モードで expand 指定 | ONNX モードを使う |
| `--expand-threshold must be a finite value in (0.0, 100.0]` | 範囲外 / NaN / inf | 有限値かつ `0 < v <= 100` を指定 |
| `--onnx-eval-scale must be a positive finite value` | 0 以下 / NaN / inf | 正の有限値（通常 600.0）を指定 |
| `... must point to different directories` | 出力系ディレクトリの同一指定 | 別ディレクトリを指定 |
| `--qsearch-leaf-replacement-output requires --qsearch-leaf-label` | replacement のみ指定 | `--qsearch-leaf-label` を併用 |
| `Output path is a symlink` | 既存出力が symlink（リンク先を問わず truncate を拒否） | symlink を削除するか別ディレクトリを使う |
| `Output path is a hardlink to the input file` | 既存出力が入力と同じ inode（Unix） | 別ディレクトリを指定 |
| `Stale expand artifact ... resolves to the current input file` | 旧 expand 出力と現在 input が同一（多段パイプライン） | 入力を移動するか `--expand-output-dir` を変更 |
| `... path contains '='` / `non-UTF-8 characters` | マーカー非対応のパス文字 | パスをリネーム（`v1.0=alpha` → `v1.0-alpha` 等） |
| `All engine processes have failed` | 外部エンジンが全プロセス死亡 | エンジンのログ / `--usi-option` / `--engine-timeout` を確認 |

## 関連

- [rescore_hcpe](rescore_hcpe.md) — hcpe 教師の eval 付け替え（NNUE 固定 depth）
- [psv_to_hcpe3](psv_to_hcpe3.md) — PSV → dlshogi 学習用 hcpe3 / hcpe 変換
- [rescore_psv-internals.md](rescore_psv-internals.md) — 内部実装（供給パイプライン、
  完了マーカー仕様、パス安全チェック等）
- [psv-utils](https://github.com/KazApps/psv-utils) — 同等機能の Python ツール
