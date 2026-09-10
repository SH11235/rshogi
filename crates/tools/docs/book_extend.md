# book_extend

`book_extend` は YANEURAOU-DB2016 テキスト定跡 `.db` の各局面について、USI エンジンの bestmove が候補集合に無い場合だけ、その手を `count=0` の候補として追加するツールです。

既存候補の `value` / `depth` / `count` / `ponder` は変更しません。追加手の `value` は追加手を指した子局面を探索し、子局面手番側の score を反転して親局面手番側視点にします。

```text
value(追加手) = -(子局面の score cp)
```

## 使い方

```bash
cargo run -p tools --release --bin book_extend -- \
  --book input.db \
  --out extended.db \
  --engine /path/to/usi-engine \
  --engine-option "EvalFile=$SHOGI_DATA/nnue/model.bin" \
  --go "nodes 10000000" \
  --parallel 12 \
  --journal extend.jsonl \
  --resume \
  --parent-journal parent.jsonl \
  --report extend_report.md
```

## 主なオプション

| オプション | 説明 |
|------------|------|
| `--book <path>` | 入力 `.db` |
| `--out <path>` | 追加後の出力 `.db` |
| `--engine <path>` | USI エンジンの実行ファイル |
| `--engine-option <k=v>` | USI option。複数指定可 |
| `--go "<args>"` | 親局面探索と子局面ラベル探索の `go` 引数 |
| `--parallel <N>` | 並列エンジン数。既定 1 |
| `--journal <path.jsonl>` | この実行の探索結果を追記する journal |
| `--resume` | `--journal` 内の現設定一致レコードを再利用する |
| `--parent-journal <path.jsonl>` | 親局面 bestmove 検出用に再利用する既存 journal |
| `--report <path.md>` | Markdown レポート出力 |

## パス検証

`--book` / `--out` / `--journal` / `--report` は全ペアで正準パス（シンボリックリンク解決後。未作成ファイルは親ディレクトリを解決して比較）の一致を検査し、同じファイルを指す組があれば起動時にエラーで拒否します。`--journal` は実行中に追記され、`--out` / `--report` は完了時に書き出されるため、どの組が衝突しても入力 book・再開用 journal・出力のいずれかを破壊するためです。

`--parent-journal` は読み取り専用のためこの検査対象外です。前回実行の `--journal` をそのまま `--parent-journal` に渡す使い方は問題ありません。

## 処理内容

1. 各局面の親局面 bestmove を決めます。
   `--parent-journal` に `kind:"parent"` のレコードがあり、ply を除いた SFEN 3 フィールドが一致する場合は、その `bestmove` を再利用します。
   無い局面だけ `--go` で親探索し、結果を `--journal` に追記します。
2. bestmove が既存候補に含まれる局面は何も変更しません。
3. bestmove が候補に無い局面では合法性を検証します。非合法手は警告してスキップします。
4. 合法な追加候補だけ、子局面を `--go` で探索します。同じ子局面は ply を除いた SFEN 3 フィールドでまとめ、1 回だけ探索します。
5. 子局面探索値から `value` と `depth` を設定し、`count=0`, `ponder=none` の候補を追加します。

`score mate N` は `book_rescore` と同じく `±(30000 - |N|)` の cp に変換し、cp 値は `[-30000, 30000]` にクリップします。

## journal と決定性

`--journal` は JSON Lines 形式です。親探索は `kind:"parent"`、子局面探索は `kind:"child"` として追記します。各行には `go` と `engine_fingerprint` を記録します。`engine_fingerprint` は `--engine` パスの basename、エンジンバイナリ内容の SHA-256、`--engine-option` を key 昇順に正規化した文字列から作ります。同名パスでもバイナリ内容が変わると fingerprint が変わるため、エンジン差し替え後に古い journal 行を誤って再利用することはありません。

`--resume` は `go` と `engine_fingerprint` が現在の実行設定に一致する `--journal` の行だけを再利用します。`--parent-journal` は別予算の親探索結果を検出シグナルとして使うため、`kind:"parent"` と SFEN 3 フィールドだけを見ます。

出力 `.db` は入力探索順や worker 完了順に依存しません。局面は SFEN 昇順、指し手は `count` 降順から USI 昇順で書き出します。同じ journal から生成する `.db` と report は決定的です。

出力 `.db` と report は、同じディレクトリの一時ファイルへ書き切ってから rename する atomic 書き込みです。書き出し途中で中断しても、既存の出力ファイルが中途半端な内容で残ることはありません。

## report

`--report` を指定すると Markdown レポートを出力します。

- bestmove が候補集合に含まれる率 before / after
- 追加手数、非合法 bestmove のスキップ数
- 追加手 value の分布
- 追加手が局面の旧 best value を上回った cp 差の上位 20
- `--parent-journal` 再利用数と、この実行で新規に行った親探索数

## 注意

`count=0` の追加手は、count 抽選ベースの既存 probe では選ばれないマーカーです。追加直後は value グラフや逆伝播には効きますが、実戦選択への反映は value ベース選択へ切り替える段階で行います。

### 探索の受信期限

探索中の `info` 行は受信期限を延長しません。期限に達すると `stop` を送り、最大 10 秒応答を回収します。期限後に `bestmove` が届いてもタイムアウトとして扱い、途中の評価を正常な journal 結果として保存しません。
探索失敗後はその worker のエンジンを再利用しません。保存済み journal は保持され、未完了タスクは `--resume` で再開できます。
