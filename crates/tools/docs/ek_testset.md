# ek_testset

`export-hcpe` は共通の [38 byte HCPE形式](hcpe-format.md) を出力します。bestMove16は指し手なしの0、dummyも0です。

`ek_testset` は、held-out CSA 棋譜から入玉局面の評価テストセットを作るツールです。
native NNUE の静的評価に加え、hcpe へ export して `yardstick_label` → `yardstick_score` で
探索ラベル品質を採点できます。

局面には 2 種類のラベルを付け、それぞれ別の指標系で採点します:

- **DT (Declaration Truth / 宣言真値)**: `Position::declaration_win` が「手番側の宣言勝ち成立」と
  判定した局面に付く「手番側勝ち」ラベル。ルールベースの判定なので ground truth として扱える。
  評価関数が勝ち確定局面を正しくプラスに読めるかを測る。
- **OC (Outcome Calibration / 勝敗較正)**: 実対局の最終結果を手番側視点の win/loss/draw にしたラベル。
  評価値から作る予測勝率 `sigmoid(eval/scale)` が実スコアとどれだけ整合するか（較正）を測る。
  draw は入玉の変換失敗（千日手・持将棋）の署名そのものなので既定で採点対象に含め、
  期待スコア 0.5 として cross entropy / Brier / calibration に算入する（符号一致率は勝敗のみ）。

## build

```bash
cargo run -p tools --release --bin ek_testset -- build \
  --input "$HOME/data/floodgate/extracted/band" \
  --out-dir runs/ek_testset/band \
  --min-ply-from-entry -20 \
  --sample-stride 4
```

出力:

| ファイル | 内容 |
|---|---|
| `testset.jsonl` | 1 行 1 局面。`sfen`, `stm`, `ply`, `source_csa`, `is_declarable`, `dt_label`, `oc_label`, `floodgate_eval_cp` |
| `sfens.txt` | `testset.jsonl` と同順の SFEN |
| `meta.json` | 入力、件数、パラメータ（生成元 CSA はレコードごとの `source_csa` に記録） |

core が `entering_king_point_info` を公開していないため、点数系フィールド
（`points_stm`, `king_in_enemy_stm`, `enemy_zone_pieces_stm`）は出力しません。DT ラベルは
`Position::declaration_win(EnteringKingRule::Point27)` で作ります。

## export-hcpe

```bash
cargo run -p tools --release --bin ek_testset -- export-hcpe \
  --testset runs/ek_testset/band/testset.jsonl \
  --out runs/ek_testset/band/testset.hcpe \
  --drop-draw false \
  --allow-missing-eval true
```

`build` の出力には `floodgate_eval_cp` が `null` のレコードが通常含まれます
（終局局面のサンプルと、`'**` 評価コメントの無い手。定跡手・評価値を出さない
エンジンなど）。そのため既定（`--allow-missing-eval false`）ではエラーで止まります。
欠損レコードを除外して変換するには上記のように `--allow-missing-eval true` を
指定してください。

`yardstick_label` が読む cshogi HuffmanCodedPosAndEval（38B/レコード）へ、入力順を保って
逐次変換します。局面は既存の HCP packer を使い、`floodgate_eval_cp` は手番側視点の i16 LE
として保存します。i16 範囲外は clamp し、件数を標準エラーへ表示します（教師値スケールの
汚染検知が目的のため、`--drop-draw` で出力から除外されたレコードの分も計上する入力側の
件数です）。
`bestMove16` は指し手なしを表す 0、`gameResult` は `oc_label` と `stm` から絶対視点へ変換、
padding は 0 です。

保存 eval は `yardstick_label`（stage 1）が eval_band / mate_ref の生成に読み、
`yardstick_score`（stage 2）がそれを mate 除外・`a_ref` 較正・参照系指標に使うため、
0 埋めすると採点を静かに歪めます。`floodgate_eval_cp` がない、
または `null` のレコードは既定で行番号付きエラーです。`--allow-missing-eval true` を
指定した場合のみ該当レコードを**出力から除外**して続行し、除外件数
（`eval_missing_skipped`）を標準エラーへ表示します。

出力は `<out>.partial` へ書き、fsync してから正常完了時のみ最終パスへ rename します
（中断時の途中書きが正常な hcpe サイズのまま残らないようにするため。途中失敗した
`.partial` も削除します）。入力と出力（`.partial` 含む）が同一実体の場合、および出力が
symlink の場合は truncate する前にエラーで拒否します。

`--drop-draw` の既定値は `false` です。標準エラーの `draw` は入力で検出した draw 件数なので、
`true` のときは出力件数に含まれません。`stm` と SFEN の手番が一致しないレコードは、
誤った教師値を作らないため行番号付きエラーにします。この検証は `--drop-draw` /
`--allow-missing-eval` の除外判定より先に行うため、除外対象のレコードでも迂回されません。

## eval

```bash
cargo run -p tools --release --bin ek_testset -- eval \
  --testset runs/ek_testset/band/testset.jsonl \
  --eval-file "$SHOGI_DATA/nnue/model.bin" \
  --progress-file "$SHOGI_DATA/progress/progress.bin" \
  --progress-buckets 8 \
  --out runs/ek_testset/band/metrics.json
```

`--progress-buckets` は学習時の routing bucket 数を指定します。`--progress-buckets 1` は
常に bucket 0 を選ぶ no-op routing のため、その場合のみ `--progress-file` は省略できます。

標準出力と `--out` に同じ JSON を出します。評価値は手番側視点 cp として扱い、DT は宣言可能局面の
符号一致率と +600cp 超過率、OC は実対局結果との符号一致率（勝敗のみ）、cross entropy、Brier、
予測勝率を [0,1] で 10 等分した等幅ビンの calibration（ビンごとの平均予測勝率と実スコア率。
draw は 0.5 として算入）を出します。calibration は全予測を保持せずビンごとの逐次集計で求めるため、
ピークメモリは入力件数に非依存です。符号一致率では `eval == 0` を一致に含めず、DT/OC とも
不一致として扱います。

`--scale`（既定 600 = いわゆるポナンザ定数）は cp→予測勝率の変換定数です。600 が現代の
評価関数に対して適切かは議論があるため絶対較正の解釈には注意し、モデル間・学習前後の比較では
**同一の scale に固定**して差分を見ます（符号一致率と DT 指標は scale に依存しません）。
cross entropy を最小化する scale を数点スキャンして実測 fit するのが確実です。
