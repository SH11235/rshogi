# rshogi-dl-usi

policy/value モデルを使う、単一探索 worker の USI エンジンです。basic形式または `stm-tokens-v1` 形式の ONNX モデルと、ONNX Runtime API 24 対応の共有ライブラリが必要です。モデルは同梱していません。

```bash
cargo build --release -p rshogi-dl-usi --features onnx
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so
target/release/rshogi-dl-usi
```

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
| NNCacheMiB | 0 | raw policy/value キャッシュの上限 MiB。0 は無効、最大1024 |
| TreeMemoryMB | 16 | node / edge arena の上限 MiB |
| CpuctMilli | 1500 | PUCT の係数の千倍 |
| PolicyTemperatureMilli | 1000 | 合法手 softmax temperature の千倍 |
| MaxGamePly | 512 | 開始局面の手数を含む引き分けの上限 |
| MoveOverhead | 50 | 時計から差し引く余裕 ms |
| EnteringKingRule | CSARule27 | 既存 core の入玉宣言ルール |
| Threads | 1 | 探索 worker 数。1 のみ |

`go nodes`、`go movetime`、`go btime/wtime` と `binc/winc`、`byoyomi`、`go infinite`、`stop` に対応します。nodes は完了した MCTS simulation の数です。depth は読み筋の長さ、hashfull は arena の使用割合で、NNUE の αβ探索とは意味が異なります。評価要求数は `info string evaluations`、実際の backend 呼び出し数は `nn_calls`、ヒット数は `cache_hits`、cache の確保量は `cache_bytes` に出力します。

ponder、MultiPV、定跡、`go depth`、`go mate`、`go searchmoves` には対応しません。対応外の go パラメータを黙って無視せずエラーにします。`go infinite` は木が満杯や終局でも stop まで bestmove を待ちます。通常の go はメモリ上限で早期に終わる場合があります。

GUI、既存の tournament、CSA client からは通常の外部 USI エンジンとして使用します。`ModelFile` 等を個別 USI option として指定してください。NNUE の EvalFile や edition の指定は不要です。局面の手順を保つには `position startpos moves ...` または開始 SFEN と moves を渡してください。

`isready` はモデルの読み込みと一回の warmup を完了してから readyok を返します。内部の引き分け評価を USI の終局通知として送りません。合法手がある root では fallback を返すため、対局の総手数上限や千日手の裁定は runner 側と揃えてください。

`NNCacheMiB` は選択された形式の全入力を照合し、合法手フィルタや temperature を適用する前の出力を保持します。木は go ごとに作り直しますが cache は同じ対局中に保持します。`usinewgame` / `gameover` と option 変更で破棄します。`isready` で指定容量を確保して初期化するため、その分の常駐メモリが必要です。上限は key・出力・metadata・特徴量 buffer に適用し、allocator の管理領域やモデル・ORT・探索木のメモリは含みません。固定訪問数の結果を保つための機構であり、時間制限では完了訪問数が変わり得ます。

`stm-tokens-v1` モデルは int64 の `tokens:[B,96]` を受け、同じ policy2187 / value1 を返します。81マスと14持駒を手番視点に正規化し、最後に元の先後を示す token を置きます。履歴は入力に含めず、千日手・連続王手などの裁定は探索側で行います。metadata に以下の3項目が必要です。未知形式・metadata 不整合・shape/dtype の不一致は読み込みエラーになります。

| metadata | 値 |
|---|---|
| rshogi.input_format | stm-tokens-v1 |
| rshogi.policy_format | direction-destination-2187-v1 |
| rshogi.value_format | stm-expected-score-sigmoid-v1 |

value は手番側の期待スコア（勝ち1、引き分け0.5、負け0）です。モデルの学習ルールと `EnteringKingRule` 等の設定を揃えて使ってください。token形式のモデル定義・export は [Python package](../../python/rshogi-dl/README.md) を参照してください。USI の形式自動選択は CPU/CUDA に対応します。既存 tools の basic 専用 API では token モデルを受理しません。
