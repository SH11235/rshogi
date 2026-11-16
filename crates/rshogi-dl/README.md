# rshogi-dl

将棋の policy/value 推論を共有するライブラリです。`features` は dlshogi 互換の基本形式で、入力は `[B,62,9,9]` と `[B,57,9,9]`、出力は 2187 個の policy logits と手番側の勝率です。入玉特徴量を含む別形式のモデルは受け付けません。

`Evaluator` は盤面単独の推論だけを行います。合法手への softmax、手順履歴による終局判定、探索統計は呼び出し側の責務です。NNUE の評価ファイルには対応しません。

`cache::CachedEvaluator` は基本入力の全要素と生の出力を保持する容量固定の wrapper です。0 MiB なら入力作成を省いて委譲します。モデル・provider を所有する evaluator と一緒に構築し、差し替え時は wrapper ごと作り直します。履歴依存の終局値や探索統計は保存しません。基本入力だけに依存し、同じ入力に同じ出力を返す backend に使用してください。

既定 feature は GPU や ONNX Runtime に依存しません。`onnx` feature で ORT backend を利用できます。`ORT_DYLIB_PATH` に ONNX Runtime API 24 対応の共有ライブラリを指定してください。`OnnxConfig` の device が非負なら CUDA、負なら CPU を利用します。TensorRT は明示的に選択し、engine cache は利用するモデルと実行環境に対応する場所へ保存してください。

入力名・shape・dtype と出力の shape を検証し、value は有限な `[0,1]`、policy は有限な logits を要求します。value に sigmoid が既に適用されたモデルを使用してください。モデルの意味は shape だけでは判別できないため、取得元の仕様も確認してください。

SFEN ごとの特徴量、全合法手ラベル、推論出力を JSONL で取り出せます。

```bash
cargo build --release -p rshogi-dl --features onnx --example evaluate
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so
target/release/examples/evaluate /path/to/model.onnx 0 < positions.sfen
```

モデルの配布は含みません。モデルの取得・利用・再配布条件は各配布元で確認してください。

## 有限回数の性能計測

`benchmark` example は、SFEN の読み込み、evaluator の構築、warmup、反復処理の時間を分けて JSON に出します。標準入力は最大 1024 行・各行 4096 bytes で、空なら初期局面を使います。入力の解析は反復時間に含みません。`--iterations` は corpus 全体を回す回数、`--warmup` は測定前に回す回数です。各区間の要求数は最大 1000 万です。

```bash
cargo build --release -p rshogi-dl --example benchmark
target/release/examples/benchmark encode-tokens --iterations 10000 < positions.sfen
target/release/examples/benchmark cache-tokens --cache-mib 64 --iterations 10000 < positions.sfen

cargo build --release -p rshogi-dl --features onnx --example benchmark
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so
target/release/examples/benchmark onnx --model /path/to/model.onnx --device 0 --batch-size 8 --warmup 10 --iterations 100 < positions.sfen
target/release/examples/benchmark onnx-cache --model /path/to/model.onnx --device 0 --cache-mib 64 --iterations 100 < positions.sfen
```

| mode | 計測対象 |
|---|---|
| `encode-basic` / `encode-tokens` | 基本特徴量 / token の作成のみ |
| `cache-basic` / `cache-tokens` | 安価な決定的合成 evaluator を使った cache 処理。NN 推論速度ではありません |
| `onnx` | 実モデルの入力作成・ORT 呼出し・CPU 出力取得。`--batch-size` は最大 256 |
| `onnx-cache` | 実モデルを包んだ cache 処理。batch size は 1 |

cache mode の `cold` は warmup 後に全 entry を消してから corpus を 1 回回した時間、`measured` は続けて corpus を反復した時間です。消去時間は含めません。corpus 内の同じ入力や hash 衝突によって hit/miss が変わるため、区間名だけで判断せず `cold_stats` / `measured_stats` の実要求数・backend 呼出し数・hit 数を確認してください。`--cache-mib 0` は cache を使わない対照です。

共通の既定値は `--iterations 100 --warmup 10 --batch-size 1 --cache-mib 64` です。ONNX mode の既定は `--device -1` (CPU)、`--cuda-tf32 true` で、`false` は CUDA TF32 を無効にします。`onnx` の末尾 batch は corpus の残り件数になるため、指定した batch size で測るときは入力件数をその倍数にしてください。

`setup_seconds` は evaluator / buffer の構築時間、`input_seconds` は標準入力待ちと SFEN 解析時間です。`onnx-cache` のモデル読込時間は `model_setup_seconds` として別に出します。`measured.seconds` は同期した推論結果が CPU に返るまでを含む壁時計時間であり、GPU kernel 単独の時間ではありません。JSON の `checksum` は出力を消費するための値で、モデルの正当性や棋力の判定値ではありません。共有マシンの CPU 競合や GPU クロックの変動を含むため、同じ入力・build profile・実行条件で反復して比較してください。
