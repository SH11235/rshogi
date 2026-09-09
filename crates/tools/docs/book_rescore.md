# book_rescore

`book_rescore` は YANEURAOU-DB2016 テキスト定跡 `.db` の各候補手に、USI エンジンの探索評価値または dlshogi ONNX value head の静的評価値を付け直すツールです。

入力定跡の局面・採択回数は保持し、候補手ごとの `value` と `depth` を更新します。出力順は決定的で、局面は SFEN 昇順、指し手は `count` 降順から USI 昇順です。

## 使い方

### USI 探索モード

```bash
cargo run -p tools --release --bin book_rescore -- \
  --book input.db \
  --out rescored.db \
  --engine /path/to/usi-engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --go "nodes 100000" \
  --journal rescored.jsonl \
  --report rescored_report.tsv \
  --parallel 1
```

### ONNX 静的評価モード

```bash
cargo run -p tools --release --bin book_rescore -- \
  --book input.db \
  --out rescored_static.db \
  --dlshogi-onnx-model /path/to/dlshogi_model.onnx \
  --onnx-gpu-id 0 \
  --eval-scale 600.0 \
  --journal rescored_static.jsonl \
  --report rescored_static_report.tsv
```

## 主なオプション

| オプション | 説明 |
|------------|------|
| `--book <path>` | 入力 `.db` |
| `--out <path>` | value/depth 更新後の出力 `.db` |
| `--engine <path>` | USI エンジンの実行ファイル。`--dlshogi-onnx-model` と排他 |
| `--engine-option <k=v>` | USI option。複数指定可（USI 探索モード） |
| `--go "<args>"` | `go` に渡す引数。例: `nodes 100000`, `depth 15`（USI 探索モード） |
| `--dlshogi-onnx-model <path>` | dlshogi 形式 ONNX value head。`--engine` と排他 |
| `--onnx-batch-size <N>` | ONNX 推論バッチサイズ。既定 1024（GPU 推論の実測で 512〜2048 はフラット、256 は −13% 遅い。CPU 推論・省メモリ環境では小さい値も検討） |
| `--onnx-gpu-id <id>` | ONNX 推論の GPU ID。`-1` で CPU。既定 0 |
| `--onnx-tensorrt` | ONNX 推論で TensorRT EP を使う |
| `--onnx-tensorrt-cache <dir>` | TensorRT エンジンキャッシュ保存先 |
| `--eval-scale <float>` | winrate→cp 変換スケール。既定 600.0 |
| `--journal <path.jsonl>` | 探索結果の追記 journal |
| `--resume` | journal 済み局面を再探索しない |
| `--report <path>` | TSV レポート出力 |
| `--parallel <N>` | 並列エンジン数。既定 1 |
| `--no-parent-search` | 親局面探索を無効化。既定有効 |

## 進捗表示

探索タスク（親局面・子局面の一意な探索数）を分母に、進捗を stderr へ表示します。`go nodes N` × 局面数で数時間規模になり得るため、% / 処理速度（pos/s）/ 残り時間 / 完了予定時刻を出します。TTY ではプログレスバー、非TTY（ログリダイレクト時）では `[book_rescore] ...` の 1 行ログを定期的に出力します（`rescore_psv` と共通の `tools::progress` を使用）。分母は `--resume` で skip された局面を除いた「実際に探索するタスク数」です。

## value の規約

各候補手は、親局面にその手を適用した子局面を探索して評価します。

```text
value(move) = -(子局面の score cp)
```

これにより `.db` の `value` は親局面の手番側視点になります。`score mate N` は `±(30000 - |N|)` の cp に変換し、cp 値は `[-30000, 30000]` にクリップします。

ONNX 静的評価モードでも評価対象は同じく子局面です。ply を除いた重複子局面をまとめて dlshogi ONNX value head で batch 推論し、返ってきた子局面の手番視点 cp を反転して親局面の手番側視点の `value` にします。

静的評価モードでは探索を行わないため、出力 `.db` の `depth` は常に `0` です。`depth=0` は「探索深さ 0」ではなく「静的評価で付与された値」の marker として扱ってください。

使い分けの目安:

- USI 探索モード: エンジン探索込みの評価値・親局面 bestmove agreement を取りたい場合。
- ONNX 静的評価モード: 大量の定跡候補を探索なしで高速に DL value head で粗く並べ替えたい場合。

## 終了動作（ONNX 静的評価モード）

ONNX 静的評価モードの正常終了は、出力 `.db` / report / journal を flush した後に通常のプロセス teardown を踏まず即時終了します（`_exit`）。ONNX Runtime（TensorRT EP）をロードしたプロセスは exit 時のライブラリデストラクタ連鎖が glibc ヒープを壊し、正常完了後に `corrupted double-linked list` abort（exit code 134）を間欠的に起こすため、その回避です。出力は即時終了前に書き終わっており欠けません。USI 探索モードの終了動作は変わりません。

## journal と決定性

出力 `.db` の局面行は、入力の full SFEN（末尾 ply を含む）単位で保持します。同じ盤面・手番でも末尾 ply が異なる `sfen` 行は別 entry として出力されます。

探索 cache のキーは ply を除いた SFEN です。同じ親局面や同じ子局面に合流する候補手は 1 回だけ探索され、結果は journal に追記されます。

journal は JSON Lines 形式です。各行には `kind`, `sfen`, `go`, `engine_fingerprint` と、子局面なら `value`/`depth`、親局面なら `bestmove` を記録します。USI 探索モードの `engine_fingerprint` は `--engine` パスの basename と、`--engine-option` を key 昇順に正規化した文字列から作ります。

ONNX 静的評価モードでは journal の `go` は常に `static` です。`engine_fingerprint` は ONNX モデルファイルの basename と `--eval-scale` から作ります。モデルファイル名または eval-scale を変えた場合、同じ journal ファイル内の古い行は stale として無視されます。

`--resume` は `go` と `engine_fingerprint` の両方が現在の実行設定と一致する journal 行だけを再利用します。`--engine` や `--engine-option`（例: `EvalFile`）を変えた場合、同じ journal ファイル内の古い行は stale として無視され、再探索されます。

ONNX 静的評価モードでは、journal への追記を `--onnx-batch-size` 単位で行います（1 batch 推論するごとにその結果を追記）。完了済みの batch は journal に残るため、静的評価中に中断しても `--resume` で未処理の batch から再開できます。中断した batch 内の途中結果は all-or-nothing で保存されません。

USI エンジンの探索結果自体は実行環境により揺れる可能性があります。再現性が必要な場合は、同じ journal と同じ探索設定で `--resume` を使ってください。同一 journal から生成する `.db` と report の内容・順序は決定的です。

## report

`--report` を指定すると TSV を出力します。

- 親局面ベース: エンジン bestmove が定跡候補に含まれる率、count 筆頭手と一致する率
- 指し手ベース: 局面内の `value_top - value` が 100/200/300cp 以上の手数
- 全体、先手番局面、後手番局面を分けて集計

ONNX 静的評価モードでは親局面 bestmove 探索を行わないため、親局面ベースの bestmove agreement セクションは出力しません。指し手ベースの value-gap 分布と先手番/後手番別 breakdown は USI 探索モードと同じ形式で出力します。

### 探索の受信期限

探索中の `info` 行は受信期限を延長しません。期限に達すると `stop` を送り、最大 10 秒応答を回収します。期限後に `bestmove` が届いてもタイムアウトとして扱い、途中の評価を正常な journal 結果として保存しません。
探索失敗後はその worker のエンジンを再利用しません。保存済み journal は保持され、未完了タスクは `--resume` で再開できます。
