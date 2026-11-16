# rshogi-dl (Python)

rshogi の policy / value Transformer のモデル定義と、検証用の smoke・ONNX export・出力照合をまとめた
package です。import 名は `rshogi_dl`。

- 入力は engine が作る 96 個の token 列 (`stm-tokens-v1`) です。この package は SFEN を token に
  変換しません。token と合法手 label は engine 側の encoder が作ります。
- ここにある CLI は、学習経路と engine への接続が正しく動くことを確かめるためのものです。
  本番の学習器ではなく、出力される checkpoint / ONNX は全て `validation-only` と記録されます。
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
