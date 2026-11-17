# rshogi-dl

将棋の policy/value 推論を共有するライブラリです。`features` は dlshogi 互換の基本形式で、入力は `[B,62,9,9]` と `[B,57,9,9]`、出力は 2187 個の policy logits と手番側の勝率です。入玉特徴量を含む別形式のモデルは受け付けません。

`Evaluator` は盤面単独の推論だけを行います。合法手への softmax、手順履歴による終局判定、探索統計は呼び出し側の責務です。NNUE の評価ファイルには対応しません。

`cache::CachedEvaluator` は宣言された入力形式の全要素と生の出力を保持する容量固定の wrapper です。基本形式は全 plane の bit 列、token 形式は全 96 byte を key にし、hash が衝突しても完全入力を照合します。0 MiB なら入力作成を省いて委譲します。モデル・provider を所有する evaluator と一緒に構築し、差し替え時は wrapper ごと作り直します。履歴依存の終局値や探索統計は保存しません。宣言した入力だけに依存し、同じ入力に同じ出力を返す backend に使用してください。

`InputBuffer::encode` は局面の完全入力を作り、読み取り専用の `EncodedInput` を返します。
`Evaluator::evaluate_encoded` が対応する backend では、cache miss の入力を再作成せず推論に使います。
未対応 backend は出力を変更せず `None` を返し、cache は encode 元の同じ局面を旧 `evaluate` へ渡します。
形式不一致や推論失敗はエラーを返し、旧 API では再試行しません。buffer を再 encode する前に view の使用を終えてください。
`CachedEvaluator` 自体の `evaluate_encoded` は未対応の `None` を返します。backend への直接転送は cache 照合と統計を迂回するため、cache を使う呼出し側は `evaluate` に局面を渡してください。

`InputBatchBuffer::new(format, capacity)` は同じ形式の平坦な入力配列を一度確保します。`push` で局面を encode するか、`push_encoded` で既存の完全入力を追加し、`view()` の `EncodedBatch` を `evaluate_encoded_batch` に渡します。value は局面数、policy は局面数 × 2187 要素を用意します。空の batch は空の出力で受理します。buffer を変更する前に view の使用を終えてください。

batch 推論への対応は `encoded_batch_capacity()` で確認します。`input_format()` の宣言だけでは batch 対応を意味しません。ORT backend は設定した最大 batch size まで対応し、全出力の shape と有限性を確認してから出力へコピーします。形式・容量・出力長の不一致と推論失敗はエラーになり、呼出し側の出力は変更しません。

cache を使う batch 呼出しは `CachedEvaluator::with_batch_capacity(inner, cache_mib, capacity)` で準備します。`cache_mib = 0` でも backend に入力形式と batch 対応の宣言を必要とします。全入力を照合し、miss だけを一度の backend 呼出しにまとめます。重複する miss の除去は行いません。全 miss 出力を検証してから要求順に cache 登録し、失敗時は cache と呼出し側の出力を保持します。`new()` は batch 領域を確保せず、従来の scalar API を使用します。

`CachedEvaluator::allocated_bytes()` は scalar cache の MiB 予算に含む領域です。追加の全出力 staging・miss 入力・key・索引は `CachedEvaluator::batch_allocated_bytes()` で別に確認できます。呼出し側が所有する入力領域は `InputBatchBuffer::allocated_bytes()` で確認でき、これらの wrapper の計数には含まれません。`CacheStats.requests` と `hits` は局面数、`nn_calls` は実際の backend 呼出し回数です。batch では `requests = hits + nn_calls` にはなりません。`nn_positions()` は backend に渡した局面数で、失敗した呼出しも計数します。`reset_stats()` と `clear()` はこれらすべてをリセットします。失敗時の `requests` は検証を通った試行局面数を含み、`hits` は正常に返した局面だけを数えます。

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
target/release/examples/benchmark onnx-batch-cache --model /path/to/model.onnx --device 0 --batch-size 8 --cache-mib 64 --iterations 100 < positions.sfen
```

| mode | 計測対象 |
|---|---|
| `encode-basic` / `encode-tokens` | 基本特徴量 / token の作成のみ |
| `cache-basic` / `cache-tokens` | 安価な決定的合成 evaluator を使った cache 処理。NN 推論速度ではありません |
| `onnx` | 実モデルの入力作成・ORT 呼出し・CPU 出力取得。`--batch-size` は最大 256 |
| `onnx-cache` | 実モデルを包んだ cache 処理。batch size は 1 |
| `onnx-batch-cache` | 入力作成・batch cache 照合・miss の一括推論。`--batch-size` は最大 256 |

cache mode の `cold` は warmup 後に全 entry を消してから corpus を 1 回回した時間、`measured` は続けて corpus を反復した時間です。消去時間は含めません。corpus 内の同じ入力や hash 衝突によって hit/miss が変わるため、区間名だけで判断せず `cold_stats` / `measured_stats` の実要求数・backend 呼出し数・hit 数を確認してください。`--cache-mib 0` は cache を使わない対照です。scalar cache mode の `--reuse-encoded false` は encoded API への転送を止め、同じ binary で cache miss の旧経路と比較します。既定は `true` で、cache off や未対応 backend では効果がありません。

共通の既定値は `--iterations 100 --warmup 10 --batch-size 1 --cache-mib 64` です。ONNX mode の既定は `--device -1` (CPU)、`--cuda-tf32 true` で、`false` は CUDA TF32 を無効にします。`onnx` / `onnx-batch-cache` の末尾 batch は corpus の残り件数になるため、指定した batch size で測るときは入力件数をその倍数にしてください。

`setup_seconds` は evaluator / buffer の構築時間、`input_seconds` は標準入力待ちと SFEN 解析時間です。`onnx-cache` / `onnx-batch-cache` のモデル読込時間は `model_setup_seconds` として別に出します。`measured.seconds` は同期した推論結果が CPU に返るまでを含む壁時計時間であり、GPU kernel 単独の時間ではありません。JSON の `checksum` は出力を消費するための値で、モデルの正当性や棋力の判定値ではありません。共有マシンの CPU 競合や GPU クロックの変動を含むため、同じ入力・build profile・実行条件で反復して比較してください。

`onnx-batch-cache` は入力 encode を反復時間に含め、再利用する全 buffer を時間計測前に確保します。`--cache-mib 0` でも同じ batch API を通ります。`cold_nn_positions` / `measured_nn_positions` は実際に backend へ渡した局面数、`batch_allocated_bytes` は wrapper の追加領域、`input_allocated_bytes` は呼出し側の入力領域です。

`model_probe` は最後の `encoded` 引数で借用入力の scalar API、`encoded-batch` で借用入力の batch API を検証できます。元の Position batch API が既定です。

```bash
cargo build --release -p rshogi-dl --features onnx --example model_probe
target/release/examples/model_probe /path/to/model.onnx 0 4 - false encoded < positions.sfen
target/release/examples/model_probe /path/to/model.onnx 0 4 - false encoded-batch < positions.sfen
```

最後に `basic` または `tokens` を追加すると、入力形式を上書きして形式不一致の拒否も確認できます。
