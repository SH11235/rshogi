# rshogi-dl-usi

policy/value モデルを使う、単一探索 worker の USI エンジンです。基本形式の dlshogi 互換 ONNX モデルと、ONNX Runtime API 24 対応の共有ライブラリが必要です。モデルは同梱していません。

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

`ModelFile` は input1 が 62 チャンネル、input2 が 57 チャンネル、policy が 2187 logits、value が sigmoid 適用済みの手番側勝率を持つモデルを指定します。対応外の shape や非有限の出力はエラーになります。

| Option | 既定 | 意味 |
|---|---|---|
| ModelFile | model.onnx | ONNX モデル |
| Device | 0 | CUDA device id。-1 は CPU |
| TreeMemoryMB | 16 | node / edge arena の上限 MiB |
| CpuctMilli | 1500 | PUCT の係数の千倍 |
| PolicyTemperatureMilli | 1000 | 合法手 softmax temperature の千倍 |
| MaxGamePly | 512 | 開始局面の手数を含む引き分けの上限 |
| MoveOverhead | 50 | 時計から差し引く余裕 ms |
| EnteringKingRule | CSARule27 | 既存 core の入玉宣言ルール |
| Threads | 1 | 探索 worker 数。1 のみ |

`go nodes`、`go movetime`、`go btime/wtime` と `binc/winc`、`byoyomi`、`go infinite`、`stop` に対応します。nodes は完了した MCTS simulation の数です。depth は読み筋の長さ、hashfull は arena の使用割合で、NNUE の αβ探索とは意味が異なります。推論回数は `info string evaluations` に出力します。

ponder、MultiPV、定跡、`go depth`、`go mate`、`go searchmoves` には対応しません。対応外の go パラメータを黙って無視せずエラーにします。`go infinite` は木が満杯や終局でも stop まで bestmove を待ちます。通常の go はメモリ上限で早期に終わる場合があります。

GUI、既存の tournament、CSA client からは通常の外部 USI エンジンとして使用します。`ModelFile` 等を個別 USI option として指定してください。NNUE の EvalFile や edition の指定は不要です。局面の手順を保つには `position startpos moves ...` または開始 SFEN と moves を渡してください。

`isready` はモデルの読み込みと一回の warmup を完了してから readyok を返します。内部の引き分け評価を USI の終局通知として送りません。合法手がある root では fallback を返すため、対局の総手数上限や千日手の裁定は runner 側と揃えてください。
