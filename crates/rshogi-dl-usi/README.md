# rshogi-dl-usi

policy/value モデルを使う、単一探索 worker の USI エンジンです。basic形式または `stm-tokens-v1` 形式の ONNX モデルと、ONNX Runtime API 24 対応の共有ライブラリが必要です。モデルは同梱していません。

```bash
cargo build --release -p rshogi-dl-usi --features onnx
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so
target/release/rshogi-dl-usi
```

policy 出力の有限性検査を短絡しない走査へ切り替える比較用 feature は
`--features onnx,policy-validation-reduction` です。既定は無効で、NaN・無限大の拒否条件は変わりません。

CUDA 推論結果を pinned host memory で受け取るには `--features onnx,cuda-pinned-output` を指定します。既定は無効です。

基本 plane 入力の CUDA 推論で1局面専用セッションを使うには `--features onnx,cuda-basic-single-row-session` を指定します。`NNBatchSize` を2以上に設定すると、初期化時にモデルをもう一度読み込みます。起動時間とメモリ使用量に影響します。

batch 探索の予約 metadata を使用量に応じて段階的に確保する、既定無効の feature は
`--features onnx,lazy-batch-metadata` です。`mcts_batch_workspace_bytes` は確保済みの実容量を示し、
探索が深まると増えることがあります。エラー後はこの workspace を破棄し、次の batch 探索で
再作成します。scalar 既定値の `NNBatchSize=1` は変わりません。

Windows では共有ライブラリを `onnxruntime.dll`、実行ファイルを `.exe` に読み替えてください。必要な CUDA / cuDNN 等の依存ライブラリも runtime の仕様に合わせて配置します。

起動後の設定例です。

```text
usi
setoption name ModelFile value /path/to/model.onnx
setoption name Device value 0
isready
position startpos moves 7g7f
go nodes 128
```

`ModelFile` は input1 が 62 チャンネル、input2 が 57 チャンネル、policy が 2187 logits、value が sigmoid 適用済みの手番側期待スコアを持つモデルを指定します。対応外の shape や非有限の出力はエラーになります。

| Option | 既定 | 意味 |
|---|---|---|
| ModelFile | model.onnx | ONNX モデル |
| Device | 0 | CUDA device id。-1 は CPU |
| CudaTF32 | true | false で CUDA TF32 を無効化。true は runtime の既定設定を維持。CPU では無効 |
| TensorRT | false | token 入力のモデルを NVIDIA TensorRT の FP16 で実行 |
| TensorRTCacheDir | 空 | TensorRT エンジンのキャッシュ先ディレクトリ |
| NNCacheMiB | 0 | raw policy/value キャッシュの上限 MiB。0 は無効、最大1024 |
| NNBatchSize | 1 | 同期推論にまとめる葉の上限。1〜256。1 は scalar 探索 |
| TreeMemoryMB | 16 | node / edge arena の上限 MiB |
| CpuctMilli | 1500 | PUCT の係数の千倍 |
| PolicyTemperatureMilli | 1000 | 合法手 softmax temperature の千倍 |
| MaxGamePly | 512 | 開始局面の手数を含む引き分けの上限 |
| MoveOverhead | 50 | 時計から差し引く余裕 ms |
| EnteringKingRule | CSARule27 | 既存 core の入玉宣言ルール |
| Threads | 1 | 探索 worker 数。1 のみ |

`go nodes`、`go movetime`、`go btime/wtime` と `binc/winc`、`byoyomi`、`go infinite`、`stop` に対応します。nodes は完了した MCTS simulation の数です。depth は読み筋の長さ、hashfull は arena の使用割合で、NNUE の αβ探索とは意味が異なります。評価要求数は `info string evaluations`、実際の backend 呼び出し数は `nn_calls`、ヒット数は `cache_hits`、cache の確保量は `cache_bytes` に出力します。

`NNBatchSize > 1` では、単一 worker が複数の未評価の葉を予約し、同期 batch 推論します。
最初の root 評価は1局面で、訪問数・時間・stop・同時に選べる葉の数によって小さい batch も発生します。
`NNBatchSize` は最大値で、毎回その局面数を推論する指定ではありません。訪問予約は完了訪問数と
区別し、`go nodes` と報告する nodes は完了した simulation 数を使います。
batch 探索は評価前の訪問予約によって葉の選択順が変わるため、同じ nodes の scalar 探索との
bestmove・PV・value の一致や、速度・棋力の改善を保証しません。既定1は scalar 経路を維持します。

batch 有効時だけ、別の `info string` 行に `nn_positions`（backend へ渡した局面数）、
`nn_batch_limit`（設定上限）、`nn_batch_mean`（nn_positions / nn_calls。呼出し0回なら0）、
`mcts_batch_workspace_bytes`（予約・入力・出力 buffer）、`cache_batch_workspace_bytes`
（cache wrapper の batch 用 buffer）を出力します。cache hit 数は局面数です。
既存の `info string nn_calls ... cache_hits ... cache_bytes ...` 行の形式は変わりません。

ponder、MultiPV、定跡、`go depth`、`go mate`、`go searchmoves` には対応しません。対応外の go パラメータを黙って無視せずエラーにします。`go infinite` は木が満杯や終局でも stop まで bestmove を待ちます。通常の go はメモリ上限で早期に終わる場合があります。

GUI、既存の tournament、CSA client からは通常の外部 USI エンジンとして使用します。`ModelFile` 等を個別 USI option として指定してください。NNUE の EvalFile や edition の指定は不要です。局面の手順を保つには `position startpos moves ...` または開始 SFEN と moves を渡してください。

`isready` はモデルの読み込みと一回の warmup を完了してから readyok を返します。内部の引き分け評価を USI の終局通知として送りません。合法手がある root では fallback を返すため、対局の総手数上限や千日手の裁定は runner 側と揃えてください。

`NNBatchSize > 1` では scalar warmup に加え、指定した最大 batch を cache の重複除去を通さず
一度推論してから readyok を返します。全 input/output が動的 batch に対応するモデルが必要です。
固定 batch のモデル、最大 batch の推論失敗、確保失敗は明示的なエラーになります。
CPU への切替、精度の切替、batch size の縮小による再試行は行いません。
option 変更は実行中の探索を stop / join した後にモデル・cache・探索器を破棄し、次の isready / go
で再構築します。探索中の isready は実行中の session を作り直しません。

`NNCacheMiB` は選択された形式の全入力を照合し、合法手フィルタや temperature を適用する前の出力を保持します。木は go ごとに作り直しますが cache は同じ対局中に保持します。`usinewgame` / `gameover` と option 変更で破棄します。`isready` で指定容量を確保して初期化するため、その分の常駐メモリが必要です。上限は key・出力・metadata・特徴量 buffer に適用し、allocator の管理領域やモデル・ORT・探索木のメモリは含みません。固定訪問数の結果を保つための機構であり、時間制限では完了訪問数が変わり得ます。

`TreeMemoryMB` は node / edge arena だけの予算です。MCTS の batch workspace と cache の
batch workspace はそれぞれ別枠で、TreeMemoryMB・NNCacheMiB の上限に含みません。
MCTS の workspace は batch 探索を始める際に確保し、arena の容量に比例する訪問予約 metadata も
含みます。NNBatchSize と TreeMemoryMB の両方が追加メモリへ影響します。
`NNCacheMiB = 0` でも batch 用 workspace は必要です。既定 NNBatchSize=1 は両方とも0 byteです。

`stm-tokens-v1` モデルは int64 の `tokens:[B,96]` を受け、同じ policy2187 / value1 を返します。81マスと14持駒を手番視点に正規化し、最後に元の先後を示す token を置きます。履歴は入力に含めず、千日手・連続王手などの裁定は探索側で行います。metadata に以下の3項目が必要です。未知形式・metadata 不整合・shape/dtype の不一致は読み込みエラーになります。

| metadata | 値 |
|---|---|
| rshogi.input_format | stm-tokens-v1 |
| rshogi.policy_format | direction-destination-2187-v1 |
| rshogi.value_format | stm-expected-score-sigmoid-v1 |

value は手番側の期待スコア（勝ち1、引き分け0.5、負け0）です。モデルの学習ルールと `EnteringKingRule` 等の設定を揃えて使ってください。token形式のモデル定義・export は [Python package](../../python/rshogi-dl/README.md) を参照してください。既存 tools の basic 専用 API では token モデルを受理しません。

`TensorRT=true` には GPU 対応の ONNX Runtime と、その TensorRT execution provider に対応する NVIDIA TensorRT ライブラリが必要です。token 入力のモデルを指定し、`Device` に CUDA device id を設定してください。TensorRT が担当しない演算には ONNX Runtime の CUDA 等が使われます。TensorRT の FP16 と通常の CUDA では推論値に差が出る場合があります。

初回の `isready` では、1〜`NNBatchSize` の入力に対する TensorRT エンジンを構築します。複数プロセスで使う場合は、モデル、設定、GPU を揃えた1プロセスで先にキャッシュを作成してください。同じ名前でモデルを差し替える場合も含め、モデルの内容、プロファイル設定、ONNX Runtime・TensorRT の版、または GPU を変更したら新しいキャッシュ先を指定してください。

`CudaTF32` は `true` / `false` を受理し、変更時はモデルと cache を再構築します。厳密な FP32 出力の照合には `CudaTF32=false` と `TensorRT=false` を使います。runtime の既定 precision は TF32 を許可する場合があるため、同じ float32 入出力でも CPU と差が出ることがあります。

Unix では正常終了時に探索 worker を stop / join し、session を破棄して標準出力・標準エラーを
flush した後、`_exit` で終了します。ORT の process-global なライブラリ終了処理を回避するためです。
致命的なエラーは非0の終了コードを返します。Unix 以外では通常のプロセス終了処理を使用します。
