# preprocess_psv

PSV の局面を qsearch の PV 末端へ置換します。`--input` と `--output` は必須です。
`--nnue` で評価モデルを指定し、省略時は駒得評価を使います。`--rescore` には `--nnue` が必要です。

LayerStacks は `--ls-bucket-mode progresskpabs|kingrank9` を指定します。
progresskpabs は `--ls-progress-buckets` と（bucket 数 1 以外で）`--ls-progress-coeff` が必要です。
Q16 は明示エラーになります。詳細は [routing 対応表](nnue-routing.md) を参照してください。
