# preprocess_psv

PSV の局面を qsearch の PV 末端へ置換します。`--input` と `--output` は必須です。
`--nnue` で評価モデルを指定し、省略時は駒得評価を使います。`--rescore` には `--nnue` が必要です。

LayerStacks は `--ls-bucket-mode progresskpabs|kingrank9` を指定します。
progresskpabs は `--ls-progress-buckets` と（bucket 数 1 以外で）`--ls-progress-coeff` が必要です。
Q16 は明示エラーになります。詳細は [routing 対応表](nnue-routing.md) を参照してください。

起動時の bucket 数・係数の組合せと入力エラー時の扱いは、[native routing の設定条件](nnue-routing.md#起動時の設定条件)を参照してください。

## 出力パスと既存ファイルの保護

入力 PSV、指定した `--nnue` と `--ls-progress-coeff` は読み取り専用資源として保護します。
その実行で係数を使用しない場合も、指定したファイルは存在する必要があります。
最終 PSV、moved mask、それぞれの `<ファイル名>.tmp` が資源や互いの alias になる指定は拒否します。
出力と一時ファイルの symlink は、リンク先が存在しない場合も拒否します。

一時ファイルは切り詰めずに開き、全 handle の実体照合に成功してから空にして書き込みます。
衝突しない既存 `.tmp` は再利用できます。照合の失敗では既存 staging の内容を維持しますが、
検査エラーで新規の空ファイルが残る場合があります。照合後に処理を開始してから失敗した場合は、
再利用したものを含め一時ファイルを削除します。mask の rename に失敗した場合は、完成した mask を
`.tmp` に残します。

公開直前にもパスを再検査するため、入力・NNUE・係数ファイルは処理完了まで移動・削除しないでください。
資源が見つからない場合は公開せず、一時ファイルを削除します。
公開は旧 mask の削除、PSV の rename、mask の rename の順です。
複数ファイルの一括更新や失敗時の復元、他プロセスによる同時変更の排他は保証しません。
