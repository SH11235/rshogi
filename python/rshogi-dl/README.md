# rshogi-dl (Python)

rshogi の policy / value Transformer のモデル定義、有限回のshard学習、検証用の smoke・ONNX export・出力照合をまとめた
package です。import 名は `rshogi_dl`。

- 入力は engine が作る 96 個の token 列 (`stm-tokens-v1`) です。この package は SFEN を token に
  変換しません。token と合法手 label は engine 側の encoder が作ります。
- `trainer`は有限回の学習・held-out評価と再開を行い、`smoke`は小fixtureで接続を検証します。
  出力される checkpoint / ONNX は、棋力・探索品質の採用前の候補として全て `validation-only` と記録されます。
  モデル構成 (`initial` / `test`) も接続確認用で、強さや速度を保証するものではありません。
- NNUE の学習には使いません。

## 準備

Python 3.11 以上。専用の仮想環境に入れてください。PyTorch は CPU 版と CUDA 版で wheel の index が
違うので、先に単独で入れます。

```bash
cd python/rshogi-dl
python -m venv .venv && . .venv/bin/activate

# CPU 版
pip install -r requirements-torch.txt --index-url https://download.pytorch.org/whl/cpu
# CUDA 版を使う場合は、環境の CUDA に対応する index (https://download.pytorch.org/whl/<cuXXX>) を指定する

pip install -r requirements.txt
pip install --no-deps -e .
```

| package | 版 | 用途 |
| --- | --- | --- |
| torch | 2.14.0 | モデル・smoke・export |
| onnx | 1.23.2 | export した graph の検査と metadata |
| numpy | 2.4.6 | ONNX Runtime との受け渡し |
| onnxruntime | 1.24.1 | export 後の実行検査・出力照合 |
| cshogi | 1.0.4 | テストの参照実装のみ (`requirements-test.txt`) |

`python -m rshogi_dl.parity --provider cuda` を使う場合だけ、`onnxruntime` の代わりに同じ版の
`onnxruntime-gpu` を入れてください。

## 出力先

成果物 (checkpoint・report・ONNX) は環境変数 `SHOGI_DATA` の下の次の 2 か所にだけ書けます。

- `$SHOGI_DATA/runs/rshogi-dl/<run>/` — smoke の checkpoint と report
- `$SHOGI_DATA/dl/transformer-v1-validation/` — 検証用の ONNX

`SHOGI_DATA` が未設定のとき、上記以外の場所、repository の中、既に存在する path は拒否します
(上書きしません)。出力先は絶対 path で指定します。

## 入力 JSONL

1 行 1 局面の JSON です。小さな fixture と接続確認のための交換形式で、大量の教師データの保存形式
ではありません。reader は 1 行ずつ読み、同時に保持するのは 1 batch 分だけです。

```json
{"tokens": [0, 0, "... 96 個 ..."], "legal_labels": [59, 68], "policy": [[59, 3], [68, 1]], "value": 0.5}
```

| key | 内容 |
| --- | --- |
| `tokens` | 整数 96 個。位置ごとに許される ID の範囲が決まっている (下記) |
| `legal_labels` | その局面の全合法手の policy label (0..2186、重複なし、最大 593 個) |
| `policy` | `null`、または `[label, weight]` の空でない list |
| `value` | 手番側から見た期待スコア。勝ち 1.0 / 引き分け 0.5 / 負け 0.0 の範囲 `[0, 1]` |

`policy` の規則:

- label は `legal_labels` に含まれるものだけ。重複不可。合法手に重み 0 を付けるのは可。
- weight は有限の非負の数で、合計が正であること。読み込み時に合計 1 へ正規化する。
- `null` は「この行は policy loss に使わない」という意味だけを持つ (終局とは限らない)。
  `null` の行に限り `legal_labels` は空でもよい。
- 空 list `[]` は不正。

次の行は読み込み時に error になります: key の過不足や重複、整数であるべき所の真偽値・小数・文字列、
`NaN` / `Infinity`、範囲外の ID、長すぎる行。`value` は読み込み時に単位を推測しません。評価値や
勝敗からの変換は fixture を作る側で済ませてください。

### token

| 位置 | 内容 | ID |
| --- | --- | --- |
| 0..80 | マス。`9 * (筋 - 1) + (段 - 1)`。後手番では盤を 180 度回す | 0 = 空、1..14 = 手番側の駒、15..28 = 相手の駒 |
| 81..87 | 手番側の持駒 (歩 香 桂 銀 角 飛 金) | `29 + offset + 枚数` |
| 88..94 | 相手の持駒 (同じ並び) | `74 + offset + 枚数` |
| 95 | 元の手番 | 119 = 先手番、120 = 後手番 |

駒の ID は 歩 1、香 2、桂 3、銀 4、角 5、飛 6、金 7、玉 8、と 9、成香 10、成桂 11、成銀 12、馬 13、
龍 14。持駒の offset は 歩 0、香 19、桂 24、銀 29、角 34、飛 37、金 40 で、枚数の上限は
18 / 4 / 4 / 4 / 2 / 2 / 4 です。手数・履歴・千日手の回数は入力に含みません。

### policy label

`81 * channel + 移動先` (移動先は token と同じ手番側視点のマス番号)。

| channel | 内容 |
| --- | --- |
| 0..9 | 移動方向: 上、左上、右上、左、右、下、左下、右下、桂左、桂右 (手番側から見た向き) |
| 10..19 | 上の 10 方向の成り |
| 20..26 | 打ち: 歩 香 桂 銀 角 飛 金 |

## 生成済み shard の読み込み

JSONL の検証・Python record の作成を毎 epoch 繰り返す代わりに、検証済みの入力を NumPy 配列の shard に変換できます。変換は全行を一度検証し、最大 `--shard-records` 行ずつ保存します。既定4096行、上限65536行で、入力全体をメモリに読みません。

```bash
python -m rshogi_dl.sharded convert \
  --source fixture.jsonl \
  --output "$SHOGI_DATA/teachers/dl/<dataset>" \
  --shard-records 4096 \
  --teacher-kind synthetic-validation-targets \
  --split fixture-only --training-status validation-only

python -m rshogi_dl.sharded verify --data "$SHOGI_DATA/teachers/dl/<dataset>"
```

dataset の出力先は `$SHOGI_DATA/teachers/dl/<dataset>/` または `$SHOGI_DATA/runs/rshogi-dl/<run>/` に限ります。絶対 path を指定し、repository・worktree 内や既存 path への出力は拒否します。`manifest.json` は全 shard の保存後に公開します。変換に失敗して manifest がない directory は未完了であり、reader は受け付けません。自動再開・上書きはしないため、別の新規出力先を使ってください。

入力JSONLは通常ファイルに限り、変換中は変更しないでください。開始時とmanifest公開直前のファイル識別子・byte数・更新時刻・metadata変更時刻を照合し、変更された場合は公開しません。manifestには入力の`source_size`と読み取った`source_records`を記録します。

各配列・metadata・一時manifestはflushとfsyncを行い、manifestをatomic renameで公開します。POSIXではshardとdatasetのdirectoryもfsyncし、ファイルシステムがdirectory fsyncを提供しない場合は失敗します。Windowsでは標準Python APIでdirectoryの永続化を確認できないため、file fsyncとatomic renameまでが保証範囲です。Windowsでコピーしたdatasetや電源断後のdatasetは、利用前に`verify`で検査してください。

| shard 内の配列 | 保存形式 |
| --- | --- |
| tokens | uint8 `[N, 96]` |
| values / policy_present | float32 `[N]` / bool `[N]` |
| legal_offsets | uint64 `[N+1]` |
| legal_indices / policy_targets | uint16 / float32 の一次元配列。各行の合法手分だけ格納 |

policy target は合法手の元の並びで合計1に正規化します。`policy=null` の行は `policy_present=false`、targetは0で、合法手が空の行も保持します。各行593手分の領域をファイルに確保する形式ではありません。global manifest は件数とshard数を保持し、個々のファイル情報とSHA-256は各shardのmetadataへ保存します。

配列はconverterが書くNPY 1.0形式です。readerは最大1024 bytesのheaderを先に検査し、期待するshape・dtype・C順序・ファイル長が一致してからmappingを作ります。空の合法手配列でも、headerに書かれた件数から先にメモリを確保しません。

manifest の `dataset_content_sha256` は、shard 順の metadata digest（各配列の checksum を含む）を
生成時にまとめた値です。入力の識別のために全 shard 一覧を保持する必要はありません。
`verify` は配列 checksum とこの digest を照合します。通常読込は生成後に変更しない契約に従い、全件 scan を省きます。

```python
import os
from pathlib import Path
from rshogi_dl.sharded import iter_batches

dataset = Path(os.environ["SHOGI_DATA"]) / "teachers" / "dl" / "<dataset>"
for batch in iter_batches(dataset, batch_size=128, drop_last=False):
    print(batch.tokens.shape)
```

reader はread-only mmapを使い、shardを順番に開き、shard境界を跨いでJSONL readerと同じ`Batch`を作ります。batchの上限は16384行です。末尾は`drop_last=True`で捨てられます。保持する作業領域は1shardのmappingと1batch分で、返したbatchは次の読込で書き換わりません。並べ替えやworker・GPU転送は行わず、CPU tensorを返します。

このreaderの対象はconverterが生成した、変更されないdatasetです。既定ではheader・shape・dtype・file sizeと読込中の置換・変更を検査し、全sampleの再検証や全ファイルのhash scanを毎epoch行いません。配布・コピー後に全ファイルを検査するときは`verify`、または`iter_batches(..., verify_hashes=True)`を明示してください。使用中のshardを編集・削除・置換せず、新しいdatasetを別directoryに生成してください。

`teacher_kind`・`split`・datasetの`training_status`は利用者が明示する出自で、未指定は`unknown`です。`training_status`は`unknown`または`validation-only`を指定できます。converterは実教師の品質を推測せず、変換成功は学習データの品質採用を意味しません。上の例は合成fixtureであり、モデル・checkpointの`validation-only`という状態は変更しません。

## モデル

token の埋め込み (語彙 121) と位置の埋め込み (96) を足し、pre-LayerNorm の encoder を通します。
attention は全 96 token 間の双方向で、畳み込みは使いません。

| 構成 | 層 | 幅 | head | FFN |
| --- | --- | --- | --- | --- |
| `initial` | 4 | 128 | 4 | 512 |
| `test` | 2 | 32 | 4 | 128 |

- policy: マス token 0..80 に共有の線形層 (幅 → 27) を掛け、`[B, 81, 27]` を channel が先に来る
  `[B, 2187]` に並べ替えます。
- value: 位置 95 の token から 1 個の logit を出します。学習時は logit のまま BCE に渡し、ONNX では
  sigmoid を付けて `[0, 1]` にします。
- loss: policy は合法手の中だけで softmax を取る soft-target cross entropy (`policy` が `null` で
  ない行の平均)、value は BCE。両者を 1:1 で足します。全行が `null` の batch では policy loss は 0 です。

### attention の実行方式

`SquareTransformer(config, attention_backend="sdpa")` は PyTorch の
`scaled_dot_product_attention` を使います。既定の `"explicit"` は QK の積・softmax・V との積を
個別に計算します。どちらも全 96 token 間の双方向 attention で、dropout は使いません。
SDPA の実際の kernel は device・dtype・PyTorch の設定に依存します。

この指定は実行方式だけを変え、model config・parameter 名・checkpoint の形式は共通です。
同じ重みを両方式へ `load_state_dict` できます。checkpoint の読込時は既定の `"explicit"` に戻るので、
SDPA を使う側でモデルを構築して重みを読み込んでください。SDPA のモデルも ONNX export 中は
標準の MatMul / Softmax で書き出し、動的 batch・metadata の検査を維持します。export 後のモデルの
実行方式は変わりません。

## 有限回の shard 学習と再開

`trainer` は shard を逐次読み、指定回数の更新試行と held-out 評価を行います。学習データと検証データは別 directory に置いてください。実教師では `teacher_kind` と `split` の明示が必要で、未知の出自や検証用 fixture を使うときは `--validation-only` を指定します。変換成功や loss の低下だけで棋力・探索品質が確認されたとは扱わないため、出力モデルは引き続き `validation-only` です。

```bash
python -m rshogi_dl.trainer \
  --train "$SHOGI_DATA/teachers/dl/<train>" \
  --validation "$SHOGI_DATA/teachers/dl/<validation>" \
  --run-dir "$SHOGI_DATA/runs/rshogi-dl/<run>" \
  --config initial --max-steps 1000 --batch-size 128 \
  --learning-rate 0.001 --weight-decay 0 --seed 0 \
  --device cuda --attention-backend sdpa --precision bf16 \
  --validation-batches 0 --checkpoint-every 250 --log-every 100
```

上の精度・実行方式は指定例です。使用するGPUで正当性と性能を確認して選んでください。CPUでは`--device cpu --precision fp32`を使い、CUDAを指定して実行できなければCPUへ切り替えず失敗します。合成fixtureでは`--validation-only`を追加してください。

| option | 内容・既定値 |
| --- | --- |
| `--max-steps` | 必須。有限のbatch更新試行数。AMPでskipした試行も数える |
| `--batch-size` | 1..4096、既定128。epoch末尾の小さいbatchも使用 |
| `--config` / `--model-config` | `initial` / `test`、または`layers/width/heads/ffn_width`のJSON object |
| `--attention-backend` | `explicit` / `sdpa`。既定explicit |
| `--execution` | `eager` / `compile`。既定eager。compile初回費用も学習時間に含む |
| `--compile-mode` | `default` / `reduce-overhead` / `max-autotune` / `max-autotune-no-cudagraphs`。既定default。非defaultはcompile時だけ指定可 |
| `--precision` | `fp32` / `tf32` / `fp16` / `bf16`。既定fp32。FP16はGradScalerを使用 |
| `--fused-optimizer` | CUDAのfused AdamWを明示選択。既定は無効 |
| `--threads` | CPU thread数、既定1 |
| `--validation-batches` | held-out先頭batchの上限、既定32。0は全件評価 |
| `--checkpoint-every` | 中間checkpointの試行間隔。既定0は最終状態のみ。中間は最大8個 |
| `--log-every` | `metrics.jsonl`への記録間隔。既定100 |
| `--deterministic` | 決定的アルゴリズムを要求。CUDAでは起動前に`CUBLAS_WORKSPACE_CONFIG=:4096:8`等を設定 |

順序は dataset の行順で、暗黙の shuffle はしません。epoch 末尾から先頭に戻り、一定 learning rate の AdamW で更新します。`metrics.jsonl` には loss、実 optimizer 更新数、policy head の更新数、AMP skip 数、epoch と sample cursor を記録します。policy のない batch では policy head の更新数は増えません。毎 step の全 parameter コピーや CPU への loss 同期は行いません。出力先は最初から `$SHOGI_DATA/runs/rshogi-dl/` 配下の新規 directory で、既存 run や checkpoint を上書き・削除しません。

`reduce-overhead`と`max-autotune`はCUDA graphを要求するmodeで、CPUでは拒否します。この2つをCUDAで使うときは更新試行の境界をcompilerへ通知します。`default`と`max-autotune-no-cudagraphs`はCPU compileでも使えます。reportの`cudagraphs_requested`は要求した設定を示すだけで、実際にgraphが使用されたことの保証ではありません。modelだけをcompileし、lossやoptimizer更新は通常の実行経路です。

最終`report.json`の学習時間・samples/sはデータ供給、転送、更新と途中の記録・checkpoint保存を含み、setupと最終held-out評価は別時間です。held-outのpolicy CEはpolicyがある行、value BCEとBrierは全行で重み付き集計します。全行policyなしなら`policy_ce=null`です。有限windowを使った値はその先頭windowの評価であり、全datasetの品質を代表するとは限りません。

再開は別の新規runへ保存します。

```bash
python -m rshogi_dl.trainer \
  --train "$SHOGI_DATA/teachers/dl/<train>" \
  --validation "$SHOGI_DATA/teachers/dl/<validation>" \
  --run-dir "$SHOGI_DATA/runs/rshogi-dl/<resumed-run>" \
  --resume "$SHOGI_DATA/runs/rshogi-dl/<run>/resume-final.pt" \
  --config initial --max-steps 2000 --batch-size 128 \
  --learning-rate 0.001 --weight-decay 0 --seed 0 \
  --device cuda --attention-backend sdpa --precision bf16 \
  --validation-batches 0 --checkpoint-every 250 --log-every 100
```

`max_steps`は再開後の総試行数です。model・学習設定・dataset指紋・実行ライブラリ版を照合し、不一致なら更新前に失敗します。変更できるのは総試行数とcheckpoint/log間隔だけです。再開状態にはmodel、AdamW、GradScaler、定数learning rateのscheduler状態(null)、進捗、Python/NumPy/TorchとCUDA runのGPU RNGを保存します。sample cursorから対象shardへ直接進み、既読行を読み捨てません。checkpointはfsync後にatomic renameし、POSIXではdirectoryもfsyncします。Windowsの保証範囲はfile fsyncとatomic renameまでです。

FP16 の scaler は `growth_factor=2.0`、`backoff_factor=0.5`、`growth_interval=2000` に固定し、再開時も一致を要求します。動的に変わる scale と growth tracker は checkpoint から復元します。

dataset 指紋は生成済み manifest の内容 root を使い、通常の run/resume で全 shard・全 sample を走査しません。dataset は使用中に変更しないでください。train/validation の path・指紋に加えて `dataset_content_sha256` が同じ場合も拒否します。並べ替えや shard の分け方が異なる dataset 間の局面・対局の重複検出は行わないため、game 単位で分割した独立 held-out を dataset 作成側で用意してください。

`resume-final.pt` は再開専用です。別に保存する `checkpoint.pt` は既存の `export` と `parity` CLI で読める v1 形式で、元の reader の契約は変更しません。最終の model・optimizer 検査後、held-out 評価より先に両 checkpoint を保存します。評価に失敗して `report.json` が書かれない場合も、保存済み状態から再開できます。同一環境の CPU で中断前後の model/AdamW/cursor/RNG が一致することをテストしています。CUDA や異なる hardware・実行方式を跨いだ bit 一致を保証するものではありません。

## smoke

```bash
python -m rshogi_dl.smoke \
  --data fixture.jsonl \
  --run-dir "$SHOGI_DATA/runs/rshogi-dl/<run>" \
  --config initial --seed 0
```

CPU・1 thread・seed 固定で、16 step × batch 8 の更新を 1 回だけ行います (step 数と batch は固定で、
変更する flag はありません)。fixture は 8 行以上必要で、8 の倍数に満たない末尾は使いません。足りない
分は先頭から読み直します。

次を全て満たしたときだけ `checkpoint.pt` と `report.json` を書き、終了 code 0 を返します。

- 全 step の loss と勾配が有限
- 埋め込み・attention・FFN・LayerNorm・policy head・value head の各 group に非 zero の勾配が届き、
  値が更新された
- 最初の batch の loss が更新前より下がった
- 保存した checkpoint を読み直すと同じ出力になる

loss の低下は接続の確認にだけ使います。学習の品質を示すものではありません。

## 学習更新の性能計測

```bash
python -m rshogi_dl.benchmark \
  --data fixture.jsonl --config initial --device cuda \
  --execution eager --precision fp32 --batch-size 32 \
  --warmup 5 --steps 20 --window-batches 4 --threads 1 --seed 0 \
  --input-mode resident \
  --output "$SHOGI_DATA/runs/rshogi-dl/<run>/benchmark.json"
```

計測専用で、checkpoint は保存しません。JSON を標準出力へ出し、`--output` を指定した場合は新規の
JSON にも書きます。成果物の保存先・上書き拒否は smoke と同じです。`validation-only` の小さな
fixture での処理時間であり、学習品質や棋力を示しません。

- `--attention-backend explicit|sdpa`: 同じ重みで attention の実行方式を比較します。
- `--execution eager|compile`: `compile` は `torch.compile` でモデルを実行します。loss と optimizer
  の Python 呼出しは対象外です。startup と warmup の時間を別に記録します。初回コンパイルは
  最初のモデル呼出しで発生し、warmup を行う場合はその時間に含まれます。
  `--warmup 0` なら初回実行・optimizer 状態の初期化も計測区間に含みます。
- `--precision fp32|tf32|fp16|bf16`: `fp32` は TF32 無効、`tf32` は FP32 の行列演算で TF32 を許可、
  `fp16` / `bf16` は autocast を使います。FP16 は GradScaler を使い、overflow により省略された
  optimizer 更新数も出します。CUDA が無い場合や対応しない精度は失敗し、CPU へ切り替えません。
  CPU では `--device cpu --precision fp32` だけを使えます。
- `--fused-optimizer`: CUDA の fused AdamW を選びます。既定は無効。学習率と weight decay は
  `--learning-rate` (既定 0.001)、`--weight-decay` (既定 0) で固定できます。
- `--input-mode resident|stream`: 先頭最大 `--window-batches` 個の完全な batch を繰り返します。
  `resident` は計測前に device へ転送し、`stream` は同じ局面列を JSONL から再読込・転送します。
  データ全量は保持せず、resident も最大 32 batch に制限します。batch 未満の末尾は使いません。
  fixture 全体が 1 batch に満たなければ失敗します。

同期した wall time と `cuda_stream_seconds` を別々に出します。CUDA event の時間にも CPU からの
演算投入待ちが入り得るため、純粋な kernel 時間ではありません。CUDA の peak allocated / reserved
メモリは warmup 後に計測し直します。各 step の loss・全 parameter の有限性・初期/最終重みの digest
は計測区間の外で検査します。現在の loss の `nonzero` は CUDA で同期を伴います。入力 fixture は
実行中に変更しないでください。FP16 は試行した sample 数と実際に optimizer が更新した sample 数の
両方で throughput を出します。

比較時は model config、seed、入力 window の digest、batch、warmup、steps、optimizer 条件を揃え、
一度に 1 つの設定だけを変えてください。各実行は同じ seed の初期重みから始めます。共有 CPU の負荷は
wall time と CUDA event の両方に影響し得るため、CUDA event だけを根拠に最速と判定しないでください。

## export

```bash
python -m rshogi_dl.export \
  --checkpoint "$SHOGI_DATA/runs/rshogi-dl/<run>/checkpoint.pt" \
  --output "$SHOGI_DATA/dl/transformer-v1-validation/<name>.onnx"
```

opset 18、batch 軸だけが動的な ONNX を書きます。書く前に次を検査し、合わなければ file を作りません。

| 名前 | 型 | shape |
| --- | --- | --- |
| 入力 `tokens` | int64 | `[B, 96]` |
| 出力 `output_policy` | float32 | `[B, 2187]` (logit) |
| 出力 `output_value` | float32 | `[B, 1]` (sigmoid 後) |

- 畳み込みがなく、attention が MatMul / Softmax として graph に残っている
- export 時と違う batch (1 と 3) で実行でき、出力の shape と値域が正しい
- metadata が次のとおり

| key | 値 |
| --- | --- |
| `rshogi.input_format` | `stm-tokens-v1` |
| `rshogi.policy_format` | `direction-destination-2187-v1` |
| `rshogi.value_format` | `stm-expected-score-sigmoid-v1` |
| `rshogi.architecture` | `square-transformer-v1` (参考情報) |
| `rshogi.model_config` | モデル構成の JSON (参考情報) |
| `rshogi.training_status` | `validation-only` |

## 出力の照合

```bash
python -m rshogi_dl.parity \
  --checkpoint "$SHOGI_DATA/runs/rshogi-dl/<run>/checkpoint.pt" \
  --onnx "$SHOGI_DATA/dl/transformer-v1-validation/<name>.onnx" \
  --data fixture.jsonl --provider cpu
```

fixture の先頭 4 行を使い、PyTorch (CPU) と ONNX Runtime の出力を batch 1 / 2 / 4、行の並べ替え、
同一行の反復で比べます。許容差は固定で、CPU は rtol 1e-4 / atol 1e-5、CUDA は rtol 1e-3 / atol 1e-4。
CUDA の照合では `use_tf32=0` を明示し、FP32 演算を使います。engine の `CudaTF32=true` は runtime の既定設定を保ち、この固定許容差の照合対象には含めません。指定した provider で実行できなかった場合は別の provider に切り替えず失敗します。成功すると、case
ごとの最大絶対差と provider の設定を JSON で出力します。

## テスト

```bash
cd python/rshogi-dl
pip install -r requirements-test.txt
export SHOGI_DATA=/path/to/shogi-data
python -m unittest discover -s tests -t . -v
```

- `SHOGI_DATA` は必須です。テストは `$SHOGI_DATA/runs/rshogi-dl/` と
  `$SHOGI_DATA/dl/transformer-v1-validation/` の下に一時 directory を作り、終了時に消します。
- engine との突き合わせは、engine 側の binary を環境変数で渡したときだけ実行します。

| 環境変数 | 内容 |
| --- | --- |
| `RSHOGI_ENCODE_BIN` | 標準入力の SFEN を token と合法手 label に変換する binary (`crates/rshogi-dl` の example `encode`) |
| `RSHOGI_MODEL_PROBE_BIN` | ONNX を engine の推論経路で実行する binary (同 example `model_probe`)。ONNX Runtime の動的 library の指定は engine 側の設定に従う |
| `RSHOGI_LEGACY_PROBE_BIN` | basic 専用の推論 example `evaluate`。旧 API の受理・拒否も検証する |
| `RSHOGI_DL_REQUIRE_INTEGRATION` | `1` にすると、上の 3 つや cshogi が無いときに skip せず失敗させる (CI 用) |

```bash
cargo build --release -p rshogi-dl --features onnx --example encode --example model_probe --example evaluate
export RSHOGI_ENCODE_BIN=$PWD/target/release/examples/encode
export RSHOGI_MODEL_PROBE_BIN=$PWD/target/release/examples/model_probe
export RSHOGI_LEGACY_PROBE_BIN=$PWD/target/release/examples/evaluate
```

fixture を自分で作るときも `encode` の出力 (`tokens` と `labels` の label) を使ってください。

package の追加依存 `cpu` / `cuda` はそれぞれ `onnxruntime` / `onnxruntime-gpu` を選びます。同じ環境に両方をインストールしないでください。`requirements.txt` は CPU 用です。
