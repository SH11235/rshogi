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
| `--nnue <FILE>` | （必須） | labeler の NNUE モデル |
| `--fv-scale <i32>` | 0 | FV_SCALE オーバーライド（0=ヘッダ自動判定）。評価器に合わせ明示（threat/none 系=28） |
| `--ls-bucket-mode <STR>` | — | LayerStacks bucket mode。LS ビルド既定は `progress8kpabs` なので通常不要 |
| `--ls-progress-coeff <FILE>` | — | progress8kpabs 用の進行度係数。LS モデル + progress8kpabs のとき必須 |
| `--depth <i32>` | 12 | 探索深さ上限（0 以下=無制限）。`--nodes` と両方 0 は不可 |
| `--nodes <u64>` | 0 | 探索ノード数上限（0=無制限）。depth を変数にするなら 0 |
| `--hash-mb <usize>` | 128 | worker ごとの置換表サイズ（MB）。局面ごとに作り直すため過大にしない |
| `--threads <usize>` | 0 | worker スレッド数（0=全コア）。出力は thread 数非依存に bit 一致 |
| `--source <STR>` | — | 出力に付与する source ラベル（hcpe はソースを持たないので任意。例 `floodgate`） |
| `--limit <usize>` | 0 | 先頭から処理する最大レコード数（0=全件）。smoke 用 |

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
