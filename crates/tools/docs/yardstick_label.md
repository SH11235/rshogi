# yardstick_label

`yardstick_label` は、ラベル品質「物差し」のステージ 1 です。固定 held-out（棋譜由来の
hcpe。各局面に保存 eval＝教師ラベルと gameResult＝実対局結果を持つ）の各局面を、与えた
labeler（NNUE 評価器 + 固定 depth）の**決定的探索**で評価し、採点に必要な値だけを 1 行 1
局面の jsonl に書き出します。ステージ 2 (`yardstick_score`) がこの出力を読み、engine ごとに
勝率スケールを較正して per-class の WDL logloss / 参照天井 / リファレンス一致を算出します。

「engine + USI param + 固定 depth」を labeler とみなし、深さ軸（depth 選定）・param 軸
（labeler config 最適化）・engine 軸（教師比較: Threat / 非 Threat / DL水匠 / 将来の DL net）を
同一の物差しで回すための共通核です。

## ビルド（重要: モデルの architecture と一致させる）

評価器の architecture を含む edition でビルドしてください。`label_bench_positions` と同じ制約です。

```bash
# 1536 HalfKaHmMerged none（既定 feature でビルド可）
cargo build -p tools --bin yardstick_label --release

# Threat モデル（例: 1024x16x32 / 1536x16x32 の Threat）は nnue-threat と LS サイズを明示
cargo build -p tools --bin yardstick_label --release \
  --no-default-features \
  --features layerstacks-1536x16x32,nnue-threat,ft-halfka_hm_merged
```

LS サイズ slot（`layerstacks-1536x16x32` など）は同時に 1 つだけ有効にします。default feature は
1536 を含むため、別サイズ（1024 等）のモデルを使う時は `--no-default-features` で切り替えます。

ONNX labeler モード（`--onnx-model`）は `dlshogi-onnx` feature（default 有効）でビルドすれば動きます
（NNUE arch には依存しないので default build で可）。

## DL value head で静的評価する（`--onnx-model`、engine 軸比較用）

`--onnx-model` を指定すると、NNUE 探索の代わりに **DL（標準 dlshogi ONNX, DL水匠 等）の value head を
1 forward pass** で回し、その静的評価を `eval_label` にします。NNUE labeler と**同じ採点用 jsonl**を出すので、
ステージ 2 で「NNUE@depth-d vs DL水匠-static」を同一 held-out・同一実結果で apples-to-apples に比較できます
（例: Floodgate 局面集を DL水匠 で rescore し、NNUE 探索ラベルと WDL 予測精度を並べる）。

ONNX 推論には GPU 版 ONNX Runtime（+ CUDA/cuDNN、TensorRT 使用時は TensorRT）が要り、`ORT_DYLIB_PATH`
/ `LD_LIBRARY_PATH` で明示します（`label_bench_dl` と同じ）。`ort` は 1.24 系想定。

```bash
export ORT_DYLIB_PATH=/path/to/onnxruntime-linux-x64-gpu-1.24.x/lib/libonnxruntime.so
export LD_LIBRARY_PATH=/path/to/onnxruntime/lib:/usr/local/cuda/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
target/release/yardstick_label \
  --in floodgate_2025_val_r3000.hcpe --out runs/dlsuisho_floodgate.jsonl \
  --onnx-model /path/to/DL_suisho.onnx --onnx-gpu-id 0 --onnx-batch-size 1024 --onnx-eval-scale 600 \
  --source floodgate
```

ONNX モードは静的（探索深さなし）なので `--capture-depths` 不可・`--nnue`/`--depth`/`--threads` は不要、
出力は単一ファイル。GPU バッチ推論なので NNUE 探索（CPU）の数十分に対し数分で終わります。

決定性: 出力は入力順（バッチ streaming で順序保存）。CUDA EP（FP32）は **同一 ORT/driver/model なら実行間で
bit 一致**しますが、ORT/driver/cuBLAS のバージョンが変わると微小に変わりえます。TensorRT EP（FP16, `--onnx-tensorrt`）
は **bit 一致を保証しません**（FP32 とも微小に異なる）。labeler 間を比較する時は **推論 mode/ORT/driver/model を固定**
してください。なお NNUE 探索モードの「`--threads` 非依存に bit 一致」は ONNX モードには当てはまりません（別経路）。

## 使い方

```bash
target/release/yardstick_label \
  --in  /path/to/floodgate_2025_val_r3000.hcpe \
  --out runs/yardstick/threat1536_d12.jsonl \
  --nnue   /path/to/threat-full-1536.bin \
  --fv-scale 28 \
  --ls-progress-coeff /path/to/progress_hao_full_cuda.e1.bin \
  --depth 12 --nodes 0 \
  --threads 0 \
  --source floodgate
```

depth を物差しの変数にするときは `--nodes 0` で depth を binding にします（固定 depth 探索）。

## オプション

| フラグ | 既定 | 説明 |
|---|---|---|
| `--in <FILE>` | （必須） | 入力 hcpe（cshogi HuffmanCodedPosAndEval, 38B/レコード） |
| `--out <FILE>` | （必須） | 出力 jsonl（採点用フィールドのみ、入力順） |
| `--nnue <FILE>` | — | labeler の NNUE モデル。**NNUE 探索モードでは必須**（`--onnx-model` 指定時は不要・無視） |
| `--fv-scale <i32>` | 0 | FV_SCALE オーバーライド（0=ヘッダ自動判定）。評価器に合わせ明示（threat/none 系=28） |
| `--ls-bucket-mode <STR>` | — | LayerStacks bucket mode。LS ビルド既定は `progress8kpabs` なので通常不要 |
| `--ls-progress-coeff <FILE>` | — | progress8kpabs 用の進行度係数。LS モデル + progress8kpabs のとき必須 |
| `--depth <i32>` | 12 | 探索深さ上限（0 以下=無制限）。`--nodes` と両方 0 は不可 |
| `--nodes <u64>` | 0 | 探索ノード数上限（0=無制限）。depth を変数にするなら 0 |
| `--hash-mb <usize>` | 128 | worker ごとの置換表サイズ（MB）。局面ごとに作り直すため過大にしない |
| `--threads <usize>` | 0 | worker スレッド数（0=全コア）。出力は thread 数非依存に bit 一致 |
| `--source <STR>` | — | 出力に付与する source ラベル（hcpe はソースを持たないので任意。例 `floodgate`） |
| `--limit <usize>` | 0 | 先頭から処理する最大レコード数（0=全件）。smoke 用 |
| `--capture-depths <CSV>` | — | 反復深化の中間 depth を 1 回の探索で捕捉し depth ごとに別ファイルへ（L0 用）。例 `9,12,15` |
| `--onnx-model <PATH>` | — | DL ONNX value head で静的評価する ONNX labeler モード（`dlshogi-onnx` feature 要、下記） |
| `--onnx-tensorrt` | false | ONNX: TensorRT EP (FP16)。未指定は CUDA EP (FP32) |
| `--onnx-tensorrt-cache <PATH>` | — | ONNX: TensorRT エンジンキャッシュ保存先 |
| `--onnx-batch-size <usize>` | 1024 | ONNX: 1 推論あたりの最大局面数 |
| `--onnx-gpu-id <i32>` | 0 | ONNX: CUDA device id（負値で CPU） |
| `--onnx-eval-scale <f32>` | 600 | ONNX: winrate→cp 変換スケール |

## depth sweep を 1 回の探索で（`--capture-depths`）

depth 選定（L0）で複数 depth を比べるとき、`--depth 9` / `12` / `15` を別々に 3 回探索する代わりに
`--capture-depths 9,12,15` を使うと、**1 回の depth-15 探索の反復深化の副産物**として各 depth のスコアを
捕捉し、`<out>_d9.jsonl` / `_d12.jsonl` / `_d15.jsonl` の 3 ファイルに書き分けます（探索 1 回ぶんの
コストで N depth＝約 1/N の時間）。

```bash
target/release/yardstick_label \
  --in floodgate_2025_val_r3000.hcpe --out runs/threat1024_floodgate.jsonl \
  --nnue threat-full-1024-400.bin --fv-scale 28 --ls-progress-coeff progress_hao_full_cuda.e1.bin \
  --capture-depths 9,12,15 --nodes 0 --threads 0 --source floodgate
# → runs/threat1024_floodgate_{d9,d12,d15}.jsonl
```

`--nodes 0`（depth 固定）では、捕捉した中間 depth のスコアは**単独固定 depth 探索と bit 一致**します
（反復深化の depth d までの挙動は最終 depth に依存しないため）。`--nodes` でノード制限する
場合は共有ノード予算により単独探索とズレうるので、その用途では `--depth` 個別実行を使ってください。

## 出力フィールド（手番側視点）

評価値・勝敗の符号規約はすべて**手番側視点（side-to-move view）**です。hcpe の保存 eval は手番側
視点 cp（PSV `score` と同じ）、dlshogi DataLoader の value 目標も手番側視点なので、先手視点へ変換
せず素通しします（ステージ 2 もこの規約で採点）。

| フィールド | 型 | 説明 |
|---|---|---|
| `stm` | char | 手番（`b`/`w`） |
| `wdl` | float | 実対局結果（手番側視点の勝率: 勝 1.0 / 負 0.0 / 引分 0.5） |
| `eval_ref` | int | held-out 保存 eval（手番側視点 cp、教師＝リファレンスラベル） |
| `eval_label` | int | labeler の探索値（手番側視点 cp） |
| `eval_band` | string | `eval_ref` の \|cp\| 帯（`0-150`/`151-600`/`601-1500`/`1501+`/`mate`）。class は labeler 非依存に固定するため `eval_ref` で決める |
| `nyugyoku` | string | 入玉 class（`none`/`black_entered`/`white_entered`/`both_entered`） |
| `in_check` | bool | 王手局面か |
| `mate_ref` | bool | `eval_ref` が飽和域（\|cp\| >= 30000）か |
| `mate_label` | bool | labeler が詰みスコアを返したか |
| `source` | string | `--source` 指定時のみ |

gameResult（0=DRAW / 1=BLACK_WIN / 2=WHITE_WIN, 絶対視点）は手番側視点の勝率へ変換します。0/1/2
以外のレコードは採点対象外として skip します（stderr に件数）。

## 決定性と隔離

- 局面ごとに新規 `Search` を作り、1 スレッド固定（`set_num_threads(1)`）で探索します。各局面の評価は
  他局面・処理順・`--threads` から独立し、同一入力なら出力は bit 一致します（`--threads 1` と
  `--threads 0` の出力一致を確認できます）。
- 決定的 CPU ラベリングなので `--threads 0`（全コア）が既定。出力は thread 数非依存に bit 一致するため
  絞る理由はありません。

## メモリ

入力件数に対してピークメモリが線形に増えないよう streaming で処理します。producer がトークン制で
in-flight 件数を一定上限に抑え、collector が入力順へ並べ替えて逐次書き出すため、reorder buffer も
in-flight 上限でバウンドします。

## コンタミ（教師/検証/対局の非交差）

held-out（特に floodgate slice）を labeler の教師にも対局の互角局面にも混ぜないでください。混ぜると
WDL 指標が train-on-test で汚染され「accuracy は上がるが強くない」状態になります。floodgate を教師に
含む labeler を採点する場合は、教師に対し dedup した clean held-out を使ってください。
