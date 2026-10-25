# policy_rollout_psv

`policy_rollout_psv` は、種の局面 (PSV) から dlshogi 系 ONNX モデル (DL水匠等) の policy で
手を選んで指し進め、途中の局面を PSV に書き出すツールです。探索はせず、1 手ごとに policy を
1 回推論して合法手の確率から抽選します。各局面の score は同じ推論の value head (静的評価) です。

種から 1 手だけ広げる [`expand_psv_from_policy`](expand_psv_from_policy.md) と違い、種から
終局 (または手数の上限) まで進めるので、種より深い局面と種ごとに異なる道筋が得られます。

`dlshogi-onnx` feature (default) が必要です。ONNX Runtime / TensorRT の準備と環境変数
(`ORT_DYLIB_PATH`、`LD_LIBRARY_PATH`) は [rescore_psv](rescore_psv.md) と同じです。

## 実行例

```bash
cargo run --release -p tools --bin policy_rollout_psv -- \
  --input seeds-a.psv seeds-b.psv \
  --output-dir $SHOGI_DATA/teachers/rollout-v1 \
  --dlshogi-onnx-model /path/to/DLSuisho15b/eval/model.onnx \
  --onnx-tensorrt --onnx-tensorrt-cache /path/to/trt-cache \
  --rollouts-per-seed 4 --parallel 1024 --seed 1
```

## 指し進め方

- `--input` を複数指定すると、指定順に 1 ファイルずつ全量を処理します (種を混ぜません)。
- 種ごとに `--rollouts-per-seed` 本の指し進めを始めます。`--parallel` 本を同時に進め、
  進行中の全局面を 1 回の推論 (batch) にまとめます。終わった枠には次の指し進めを入れます。
- 手の選び方: 合法手の logit を softmax した確率 p のうち、`--min-prob` 未満の手を外し
  (最善手は常に残す)、`--top-k` 手に絞ってから p^(1/T) (`--temperature` が T) の重みで抽選します。
  T = 0 では常に最善手を指します。
- 種からの最初の 1 手では、種のレコードの move16 (元の対局で指された手) を候補から除きます。
  元の対局の道をなぞって既存の局面を出し直すのを避けるためです。`--allow-seed-move` で除かなくなります。
  move16 が 0 か非合法のとき、合法手が 1 つしかないときは除きません。
- 終わり方: 詰み (合法手なし)、入玉宣言 (27 点法)、千日手 (指し進めの中で同一局面 4 回。
  連続王手は王手をかけ続けた側の負け)、通し手数が `--max-game-ply` に達したとき。
  種より前の手順は見ないので、種の前の局面との千日手は数えません。

## 出力

`--output-dir/<入力ファイル名から拡張子を除いた名前>/` に chunk ごとの `rollout-NNNNNN.psv`
(`NNNNNN` は 0 始まりの chunk 番号) を書きます。1 chunk は入力の種 `--chunk-seeds` 件分で、その種から
始めた全指し進めの局面が入ります。chunk 番号 k のファイルは入力の `k * chunk-seeds` 件目からの種の分です。
拡張子を除いた名前が同じ入力を同時に渡すとエラーにします。

| フィールド | 値 |
|---|---|
| sfen | 種の局面と指し進めの途中の局面。種の局面は同じ種から複数本を始めても 1 回だけ書きます |
| score | value head の勝率を手番視点 cp に変換した値 (`cp = scale * ln(p / (1 - p))`、scale は `--onnx-eval-scale`) |
| move16 | その局面で抽選した手 |
| game_ply | 種の game_ply に、種から指した手数を足した値 |
| padding | 種から何手目か (種の局面は 0、255 で飽和)。通常の PSV では 0 のバイトなので、0 を前提にする後段 (例: `psv_dual_label`) では上書きされます |
| game_result | `--game-result zero` (既定) は 0。`rollout` は指し進めの結末を手番視点で入れる (勝ち 1 / 負け -1、千日手・手数上限は 0) |

詰みの局面・宣言できる局面・手数上限の局面など、指し手を選ばずに終わった局面は書きません。
chunk 内の局面は指し進めが終わった順に並びます。学習に使う前に `shuffle_psv` で混ぜてください。

## 再現性と再開

- 各指し進めの乱数は `--seed`・入力ファイル名・ファイル内の種の通し番号・指し進めの番号だけで決まります
  (入力を渡す順には依りません)。入力と option が
  同じなら出力は bit 一致します (TensorRT はエンジンを作り直すと数値が変わり得るので、
  `--onnx-tensorrt-cache` で同じエンジンを使ってください)。
- chunk は `rollout-NNNNNN.psv.tmp` に書いてから rename します。完成した chunk はそれだけで使えます。
  同じコマンドを再実行すると、入力ごとに完成済みの chunk を飛ばし、残りから再開します。中断した chunk は最初から作り直します。途中で option を
  変えた場合の整合は検査しないので、条件を変えるときは別の出力ディレクトリにしてください。
- メモリは入力件数に比例せず、`--chunk-seeds` 件の種と `--parallel` 本の進行中の指し進め
  (各指し進めの局面は終わるまで保持) に比例します。
  抽選用の作業領域は合法手数の上限に合わせた固定長のスタック配列で、抽選時のヒープ確保はありません。

## オプション

| オプション | 既定 | 説明 |
|---|---|---|
| `--input` / `-i` | 必須 | 種の局面の PSV (複数可) |
| `--output-dir` / `-o` | 必須 | 出力ディレクトリ |
| `--dlshogi-onnx-model` | 必須 | 標準 dlshogi ONNX モデル |
| `--rollouts-per-seed` | 1 | 1 つの種から始める指し進めの本数 |
| `--parallel` | 1024 | 同時に進める指し進めの数 (= 推論の最大 batch) |
| `--chunk-seeds` | 10000 | 1 chunk (出力ファイル・再開の単位) の種の数 |
| `--temperature` | 1.0 | 抽選の温度。0 で常に最善手 |
| `--min-prob` | 0.01 | 確率 (0〜1) がこれ未満の手を抽選から外す。0 で外さない |
| `--top-k` | 0 | 確率の上位 k 手だけから抽選する。0 で制限なし |
| `--max-game-ply` | 512 | 通し手数がこの値に達したら打ち切る |
| `--game-result` | `zero` | `zero` / `rollout` (上表) |
| `--seed` | 1 | 乱数の seed |
| `--allow-seed-move` | false | 種からの最初の 1 手で種の move16 も候補に残す |
| `--onnx-gpu-id` | 0 | CUDA device id。負値で CPU 推論 |
| `--onnx-tensorrt` | false | TensorRT EP (FP16) を使う |
| `--onnx-tensorrt-cache` | — | TensorRT エンジンキャッシュの保存先 |
| `--onnx-eval-scale` | 600 | 勝率→cp 変換のスケール |

## 出力パスの保護

入力 PSV、ONNX モデル本体、`ORT_DYLIB_PATH` のライブラリと同じ実体への出力を、推論器の
初期化前と各 chunk の作成時に拒否します。
