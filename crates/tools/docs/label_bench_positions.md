# label_bench_positions

`label_bench_positions` は、`extract_bench_positions` が出力する `label_bench*.jsonl`
の各局面を rshogi の探索で深く評価し、ground truth ラベル `eval_deep` 等を追記する
ツールです。`rescore_psv` のような PSV 経由ではなく jsonl を直接読み書きします。

教師ラベル品質ベンチ（Track 0）の ground truth 生成に使います。背景は
`docs/plans/20260613_teacher_dl_l3_roadmap.md`（T0.2）を参照。

## ビルド

```bash
cargo build -p tools --bin label_bench_positions --release
# → target/release/label_bench_positions
```

LayerStacks モデルを評価器に使うため、ビルドはその architecture を含む edition で
行ってください（既定 feature の `layerstacks-1536x16x32` 系）。

## 使い方

bullet v100-400（LayerStacks 1536x16x32 / HalfKaHmMerged / progress8kpabs）で
depth 25・500,000 nodes ラベリングする例:

```bash
target/release/label_bench_positions \
  --in  runs/label_bench/label_bench.jsonl \
  --out runs/label_bench/label_bench.deep.jsonl \
  --nnue eval/ls_halfka_hm_merged_1536x16x32_none/bullet_v100-400.bin \
  --fv-scale 28 \
  --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/progress_hao_full_cuda.e1.bin \
  --depth 25 --nodes 500000 \
  --threads 8
```

`label_bench_nyugyoku.jsonl` も同様に 1 回ずつ実行します。

## オプション

| フラグ | 既定 | 説明 |
|---|---|---|
| `--in <FILE>` | （必須） | 入力 jsonl（各行に `sfen` を含む JSON オブジェクト） |
| `--out <FILE>` | （必須） | 出力 jsonl（入力レコード + 探索結果フィールド） |
| `--nnue <FILE>` | （必須） | ground truth 評価器の NNUE モデル |
| `--fv-scale <i32>` | 0 | FV_SCALE オーバーライド（0=ヘッダ自動判定）。評価器に合わせ明示すること（v100 系=28） |
| `--ls-bucket-mode <STR>` | — | LayerStacks bucket mode。LS ビルドの既定は `progress8kpabs` なので通常は指定不要 |
| `--ls-progress-coeff <FILE>` | — | progress8kpabs 用の進行度係数（USI `LS_PROGRESS_COEFF` と同じ）。bucket mode が progress8kpabs のとき必須 |
| `--depth <i32>` | 25 | 探索深さ上限（0=無制限）。`--nodes` と両方 0 は不可 |
| `--nodes <u64>` | 500000 | 探索ノード数上限（0=無制限）。深さと併用し先に到達した方で停止。`--depth` と両方 0 は不可 |
| `--hash-mb <usize>` | 128 | 置換表サイズ（MB）。局面ごとに作り直すため過大にしない |
| `--threads <usize>` | 0 | worker スレッド数（0=利用可能 CPU 数） |

## 評価器の推論設定（重要）

LayerStacks モデルは `init_nnue` のロードだけでは正しく評価できません。USI エンジンと
同じ推論設定（FV_SCALE / bucket mode / 進行度係数）を揃える必要があります。

- LS ビルドの bucket mode 既定は `progress8kpabs` です。このとき
  `--ls-progress-coeff` を**必ず**指定してください。未指定の場合、bucket 選択が学習時と
  食い違ってラベルが静かに狂うため、ツールはエラーで停止します（安全側に倒す）。
- 係数ファイルはモデルごとに異なります。v100 系は `progress_hao_full_cuda.e1.bin`、
  v96 系は `nodchip_progress_e1_f1_cuda.bin` のように学習レシピに対応します。各モデルの
  `eval/<dir>/README.md` を確認してください。
- `--fv-scale` も評価器の native 値（v100 系は 28）を明示してください。

## 出力フィールド

入力レコードの全フィールドを保ったまま、以下を追記します。

| フィールド | 型 | 説明 |
|---|---|---|
| `eval_deep` | int | 探索値（**先手視点 USI cp**、`eval_cp_black` と同じ規約）。詰みは生のスコア（絶対値 30000 超） |
| `mate_deep` | int（任意） | 詰みのときのみ出力。先手視点の符号付き手数（正=先手が詰ます、負=先手が詰まされる） |
| `bestmove_deep` | string | 最善手（USI） |
| `pv_deep` | string[] | 読み筋（USI、L3/T0.6 のノード種別分析用） |
| `depth_deep` | int | 到達した探索深さ |
| `nodes_deep` | int | 探索ノード数（depth と nodes のどちらが binding だったか確認に使える） |

探索に失敗したレコード（不正 SFEN 等）は出力せず、stderr に理由を記録します
（出力行数 < 入力行数になりうる）。なお探索自体がパニックした場合は設定/ビルド不整合等の
致命バグとみなし、プロセスを非ゼロ終了します（壊れた巨大出力を黙って残さない）。

`eval_cp_black` は floodgate の探索値（先手視点）、`eval_deep` は本ツールの深い探索値
（先手視点）で、T0.3 でクラス別に突き合わせます。

## 決定性と隔離

- 局面ごとに新規 `Search` を作り、1 スレッド固定（`set_num_threads(1)`）で探索します。
  これにより各局面の評価は**他局面・処理順・`--threads` から独立**し、同一入力なら
  出力は bit 一致します（`clear_tt` + `clear_histories` だけでは time-management 継続用
  フィールドが前局面を持ち越し、処理順で結果が変わるため不十分）。
- `--threads 1` と `--threads 8` の出力が一致することを確認できます。

## メモリ

入力件数に対してピークメモリが線形に増えないよう streaming で処理します。producer が
トークン制で in-flight 件数を一定上限（`worker 数 × 4` 程度）に抑え、collector が入力順へ
並べ替えて逐次書き出すため、reorder buffer も in-flight 上限でバウンドします。数千万件規模の
入力でもピークは worker 数に比例した一定量に収まります（探索コスト自体は件数線形）。
