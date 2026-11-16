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
