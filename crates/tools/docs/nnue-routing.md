# tools の LayerStacks routing 対応

native 評価経路で選べる mode は `progresskpabs`（tatara の f32）、`progresskpabsq16`
（YaneuraOu / BulletOu SFNN の整数進行度）、`kingrank9` です。

対象は `preprocess_psv`、`rescore_psv`、`gensfen` の native backend、
`benchmark` の内部 API、`label_bench_positions`、`rescore_hcpe`、`yardstick_label`、
`bench_nnue_eval`、`verify_nnue_accumulator`、`ek_testset`、`nyugyoku_metrics` です。
`eval_sfens` と `nnue_saturation` は進行度方式（`progresskpabs` / `progresskpabsq16`）だけを
`--bucket-mode` で選ぶ診断ツールです。

progress 係数は USI の `LS_PROGRESS_COEFF` と同じ f64 LE の progress.bin を指定します。
同じファイルから f32 の係数と、YaneuraOu と同じ `round(w * 65536)` の Q16 係数の両方を作ります。
`progresskpabs` と `progresskpabsq16` は量子化境界で異なる bucket を選ぶことがあるため、
モデルの学習時と同じ mode を指定してください。
`verify_nnue_accumulator` が受け付ける f32 形式の係数ファイルは `progresskpabsq16` には使えません。

## progress 係数の保持と再読込み

係数はロードした値をメモリに保持し、実行中のファイル差し替えでは再読込みしません。
別の係数で処理するときは、対応する係数パスを指定してツールを再起動してください。
native探索はstackの生成・reset時に取得したsnapshotを使い、`gensfen`のbucket集計も
worker開始時の係数を保持します。係数の形式・CLI名・bucket計算式は変わりません。

## 起動時の設定条件

| mode | 格納 bucket 数 | 指定する routing bucket 数 | progress 係数 |
| --- | --- | --- | --- |
| `progresskpabs` | モデルによる | 1 以上、格納数以下で明示 | routing 数 1 では省略可、それ以外は必須 |
| `progresskpabsq16` | モデルによる | 1 以上、格納数以下で明示 | routing 数 1 では省略可、それ以外は必須（f64 形式） |
| `kingrank9` | 9 | 指定しない | 指定しない |

routing 数 1 でも係数ファイルを指定した場合は読み込みと形式検証を行います。
非 LayerStacks モデルに routing オプションを指定するとエラーになります。
`rescore_psv` は通常、routing 数と格納数の一致も要求します。意図的に異なる数を使う場合のみ
`--allow-routing-buckets-mismatch` を指定してください。

mode・bucket 数・係数の要否を検証し、係数の読み込みに成功してから routing と係数を適用します。
設定エラーや係数の読み込みエラーでは、それまでの routing と係数を変更しません。
モデルのロードや FV_SCALE 設定まで含めて元に戻す保証ではありません。
`rescore_psv` の係数 SHA は、評価に使う係数と同じ読み込み済み bytes から計算します。

CLI 名は各ツールのドキュメントを参照してください。`ek_testset` と `nyugyoku_metrics` は
`--bucket-mode` で指定します（`ek_testset` は進行度の option だけを指定して省くと `progresskpabs`）。
