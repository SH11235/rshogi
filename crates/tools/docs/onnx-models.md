# ONNX モデルの入力と推論 backend

次のツールは、モデルの metadata から基本 plane 形式と `stm-tokens-v1` 形式を選択します。
モデルの指定には既存のオプション名を使います。`dlshogi-onnx` feature は既定で有効です。

| ツール | モデル指定 | 最大 batch | cp 変換スケール |
|---|---|---|---|
| [book_rescore](book_rescore.md) | `--dlshogi-onnx-model` | `--onnx-batch-size` | `--eval-scale` |
| [yardstick_label](yardstick_label.md) | `--onnx-model` | `--onnx-batch-size` | `--onnx-eval-scale` |
| [label_bench_dl](label_bench_dl.md) | `--dlshogi-onnx-model` | `--onnx-batch-size` | `--onnx-eval-scale` |
| [policy_rollout_psv](policy_rollout_psv.md) | `--dlshogi-onnx-model` | `--parallel` | `--onnx-eval-scale` |

基本 plane 形式は dlshogi 互換の `input1:[B,62,9,9]` と `input2:[B,57,9,9]`、token 形式は
int64 の `tokens:[B,96]` を受けます。共通の出力は `output_policy:[B,2187]` と
`output_value:[B,1]` です。value は sigmoid 適用済みの手番側期待スコア `[0,1]` を使います。
policy は手番正規化した方向・移動先の logit で、tool が合法手を選びます。

`stm-tokens-v1` は次の metadata を必要とします。基本 plane 形式はこれらを持たない既存モデルも
受け付けます。tensor の名前・dtype・shape と metadata の不整合は読み込み時にエラーになります。

| key | token 形式の値 |
|---|---|
| `rshogi.input_format` | `stm-tokens-v1` |
| `rshogi.policy_format` | `direction-destination-2187-v1` |
| `rshogi.value_format` | `stm-expected-score-sigmoid-v1` |

CPU は `--onnx-gpu-id=-1`、CUDA は非負の GPU ID を指定します。基本 plane 形式では既存の
`--onnx-tensorrt` / `--onnx-tensorrt-cache` を利用できます。token 形式は CPU/CUDA のみで、
TensorRT の指定はエラーになります。ONNX Runtime は `ORT_DYLIB_PATH` で指定してください。

最大 batch が 1 を超える場合、モデルの batch 軸は動的である必要があります。value のみを使う
ツールも同じ policy/value モデルを読みますが、呼出し側への policy のコピーは行いません。
推論を入力順に返し、末尾の少数 batch も処理します。

cp 変換は `p` を `[0.001,0.999]` に収めた `scale * ln(p/(1-p))` の整数切捨てです。
既定スケールは 600 です。`label_bench_dl` は黒視点、他の value 出力は各ツールの手番視点の
規約に従います。PSV の clip や game_result、HCPE 変換時の evalfix は各ツールの設定を使います。
