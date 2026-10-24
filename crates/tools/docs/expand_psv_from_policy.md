# expand_psv_from_policy

PSV の各局面で ONNX ポリシー推論を行い、合法手の選択確率が `--threshold`（%）を超える手の
次局面を別の PSV に出力します。標準 dlshogi は default feature、AobaZero は
`aobazero-onnx` feature が必要です。実行例と全オプションは
[pack tools](pack_tools.md#expand_psv_from_policy) を参照してください。

## 出力パスの保護

入力 PSV、選択した ONNX モデル本体、`ORT_DYLIB_PATH` に指定したライブラリと同じ実体への出力を拒否します。
同一パスだけでなく hardlink と大小文字 alias を検査し、出力 symlink は参照先を問わず拒否します。
ONNX Runtime の初期化前に検査し、出力を開いた後もファイルの実体を照合してから切り詰めます。

衝突しない既存の出力ファイルは処理開始時に置き換えます。途中の失敗や中断で以前の出力へ戻す機能はなく、
staging を介さないため途中までの出力が残る場合があります。衝突検査は同時アクセスの排他を保証しません。

ONNX external tensor、実行時に追加ロードされる依存ライブラリ、TensorRT cache の各ファイルは
自動列挙していません。これらの資源とは別の出力先を指定してください。

## 出力内容

出力は入力順に各局面の展開先を列挙します。`score`、`move16`、`game_result`、`padding` は0、
`game_ply` は元の値に1を加算（上限で飽和）します。必要なら後段の `rescore_psv` でスコアを付与してください。
入力件数全体を読み込まず、`--batch-size` の単位で処理します。
