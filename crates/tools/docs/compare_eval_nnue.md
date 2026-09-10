# compare_eval_nnue

同じサンプル局面を2つの NNUE モデルで評価し、スコア差・MAE・相関を比較します。これだけで蒸留成立性や棋力を判定するツールではありません。

```bash
cargo run -p tools --release --bin compare_eval_nnue -- \
  --input "$SHOGI_DATA/teachers/sample.bin" \
  --teacher-nnue "$SHOGI_DATA/nnue/teacher.bin" \
  --student-nnue "$SHOGI_DATA/nnue/student.bin" \
  --engine /path/to/rshogi-usi --mode static --samples 1000 --threads 4 \
  --output comparison.tsv
```

## 比較する値

- `--mode search`（既定）: `go depth N` の主 PV の exact `score cp`。**depth 1 も探索**で、静的評価ではありません。単位はエンジンが USI へ出力する cp。
- `--mode static`: rshogi の `eval` コマンドによる fresh 静的評価。`info string Static eval: N` のモデル scale 適用済み raw 値を使います。USI cp と同じ単位とは仮定しません。非対応エンジンや NNUE 未ロードはエラーです。`eval` 後の `isready` 応答まで読み、次の局面と応答を混ぜません。

両モードとも手番視点です。モデルの有効 scale・bucket/routing はこのプロトコルでは取得しないため、表示と TSV に `unknown` と記録します。エンジン既定設定に依存するため、同じ単位名でもモデル間の scale 一致は保証しません。エンジン commit・build feature・有効オプション・モデル hash は実験記録へ別途保存してください。

探索の `mate N`（`+` / `-` を含む）は数値 cp に変換せず保存し、MAE・相関から除外します。`cp 31000` のような大きい通常 cp は詰み扱いしません。bound 値と主 PV 以外のスコアは採用しません。スコアなしは欠測として保存します。

旧版は depth 1 を静的評価と呼び、mate を ±31999 に変換し、絶対値30000以上を一律除外していました。古い score データは消さず、モード・型・集計条件を確認してから比較してください。新旧の統計を無条件に同じ計器として扱わないでください。

## 入出力と期限

入力は PackedSfenValue（40バイト）のファイルを複数指定できます。既存の全レコード番号を作るサンプリング方式は変更していないため、入力レコード数に比例するメモリを使います。

| オプション | 意味・既定 |
|---|---|
| `--input` / `-i` | 必須。入力ファイル（複数指定） |
| `--teacher-nnue` / `--student-nnue` | 必須。比較するモデル |
| `--engine` / `-e` | 必須。USI エンジン |
| `--mode` | `search`（既定）または `static` |
| `--depth` / `-d` | search の深さ、既定1（正数）。static では使用しない |
| `--samples` / `-s` | サンプル数、既定10000（正数） |
| `--threads` / `-t` | 並列 worker 数、既定8（正数）。各 worker に2エンジン、各エンジン Threads=1 / hash=16MB |
| `--seed` | 乱数 seed、既定42 |
| `--timeout-ms` | 起動全体と各局面の応答期限、既定120000（正数）。info が流れ続けても延長しない |
| `--output` / `-o` | TSV 保存先 |

EOF、期限超過、明示的エラー応答ではエラー終了し、共通 EngineProcess が子プロセスへ quit を送り、終了しなければ kill/wait します。診断には直近 stderr 2行を最大4096バイト/行で保持します。通信失敗を欠測へ変換して処理を続行しません。通信失敗 run では TSV を作成・更新しません（既存ファイルは残ります）。

TSV は先頭5列 `sfen, teacher_score, student_score, diff, original_score` を維持し、`mode, units, perspective, model_scale, bucket_routing, teacher_model, student_model, depth` を追加します（区切りはタブ）。mate は `mate +` 等の文字列、欠測は空欄。両方が数値のときだけ diff を保存します。詰みのみ・欠測のみの場合も行を保存します。入力の original_score はそのまま保存し、単位や視点を自動変換しません。
