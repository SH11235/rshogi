# yardstick_score

`yardstick_score` は、ラベル品質「物差し」のステージ 2 です。`yardstick_label` が出した採点用
jsonl（手番側視点で `wdl`＝実対局結果・`eval_ref`＝保存教師 eval・`eval_label`＝labeler の探索値）を
読み、engine ごとに勝率スケールを較正してから class 別に指標を出します。

複数ファイルを渡すと labeler/depth を並べて比較できます（depth sweep・config sweep・教師比較）。

## ビルド

```bash
cargo build -p tools --bin yardstick_score --release
```

採点は純粋な算術のみで NNUE 推論を含まないため、architecture feature は不要（既定ビルドで動作）です。

## 使い方

```bash
target/release/yardstick_score \
  runs/yardstick/threat1536_d9.jsonl \
  runs/yardstick/threat1536_d12.jsonl \
  runs/yardstick/threat1536_d15.jsonl \
  --out runs/yardstick/results.json
```

## オプション

| フラグ | 既定 | 説明 |
|---|---|---|
| `<labeled>...` | （必須） | 採点する jsonl（`yardstick_label` 出力）。複数指定可 |
| `--out <FILE>` | — | 結果 JSON の出力先（per-file/per-group の数値を機械可読で残す）。入力ラベル jsonl と同一パス/inode は拒否（高価なラベルの上書き防止） |

## 指標

符号規約はすべて手番側視点。詰み（labeler が詰みスコアを返した `mate_label`、または保存 eval が
飽和域 \|eval_ref\| >= 30000、加えて防御的に \|eval_label\| >= 30000）は較正・logloss・一致から除外
します（飽和域は勝率較正を歪めるため）。`spearman` は n<2・分散 0 の group で未定義になり、表では `—`、`--out` JSON では `null` です。

- **主指標: WDL logloss** = mean[ CE(sigmoid(eval/a), wdl) ]。`a` は labeler ごとに WDL logloss
  最小化で較正したスケール（1 engine につき 1 つの global scale。class ごとには較正しない＝scale を
  class に過適合させない）。NNUE の FV_SCALE と DL の winrate を混ぜても scale 差を精度と誤認しない
  ように、各 engine を win-prob 軸に合わせてから logloss を取ります。WDL logloss は NNUE 学習の
  eval+WDL 目標と同じ ground truth（探索深さと独立）を、proper scoring rule で測ったものです。
- **参照天井（ceiling）**: 保存 eval（教師）の符号一致率。labeler が超えるべき model 非依存の上限
  （datagap の `diag_strict.py` の `evalvs_result` と同義。Floodgate ≈ 0.797 / DL水匠val ≈ 0.787）。
  `ref_ceiling_all` は全レコード（詰み含む）、各 group の `ref_sgn` は採点対象（詰み除外）で算出します。
- **副指標: リファレンス一致**: 較正後 win-prob 空間で labeler と保存 eval の MAE（`wp_mae`）、および
  eval_label vs eval_ref の Spearman 順位相関（`spearman`）。深いリファレンス eval への収束効率を見ます。

## 出力

ファイルごとに、ヘッダ（`n_total` / `n_scored`(非詰み) / `a_label` / `a_ref` / `ref_ceiling(all)`）に続けて
group 別の表を stdout に出します。group は `overall` / `eval_band=*` / `nyugyoku=*` / `in_check=*` /
（source ラベルが 2 種以上あれば）`source=*`。

class は各次元の**周辺スライス（marginal）**で出します（`eval_band×nyugyoku×in_check×source` の直積
セルは出しません）。bias の所在は周辺スライスで特定でき、直積は入玉局面のほぼ無い held-out で空セルが
多発して可読性・統計的安定性を損なうためです（T0 `compare_t03.py` の各 key 別集計と同じ方針）。

```
group                        n   lbl_loss   ref_loss   lbl_sgn   ref_sgn    wp_mae  spearman
overall                  44215     0.5XXX     0.5XXX    0.6XXX    0.7XXX    0.0XXX    0.8XXX
eval_band=0-150          ...
nyugyoku=black_entered   ...
```

`--out` を付けると同じ数値を JSON で書き出します（per-file の配列、各 file に groups 配列）。

## 較正の仕組み

`win_prob = sigmoid(eval / a)`。WDL logloss（NLL）は `k = 1/a` について凸なので、`k` を黄金分割探索で
最小化します（決定的・乱数なし）。`a` の探索域は 10〜20000cp。draw（wdl=0.5）は soft target として
そのまま CE に入ります。

この sigmoid 写像は nnue_train の WRM loss（`loss_kind=wrm`、target は
`sigmoid((cp − offset)/scaling)`）の logistic family と同種で、scale を engine ごとに較正する点が要点
です。WRM の offset/scaling や pow_exp を厳密に踏襲するのではなく、proper scoring rule（logloss）で
較正済み sigmoid を測ることで、labeler 間を apples-to-apples に比較します。

## メモリ

held-out は設計上 5〜20 万局面で bounded なので、採点は全件を読み込んでから行います（億規模の教師
プールを load-all する系ツールとは別物。入力は held-out サイズで頭打ち）。
