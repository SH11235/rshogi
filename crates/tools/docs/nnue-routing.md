# tools の LayerStacks routing 対応

native 評価経路で選べる mode は `progresskpabs`（tatara の f32）と `kingrank9` です。
`progresskpabsq16`（YaneuraOu / BulletOu SFNN）は core / USI エンジンでは対応していますが、
tools の native 経路は整数係数のロードに対応していないため、明示エラーで停止します。
係数指定の有無や bucket 数 1 でも Q16 mode を受理しません。

対象は `preprocess_psv`、`rescore_psv`、`gensfen` の native backend、
`benchmark` の内部 API、`label_bench_positions`、`rescore_hcpe`、`yardstick_label`、
`bench_nnue_eval`、`verify_nnue_accumulator` です。
`eval_sfens` と `nnue_saturation` は `progresskpabs` の係数を使う診断ツールです。

Q16 モデルには対応する USI エンジンを使い、`LS_BUCKET_MODE=progresskpabsq16`、
`LS_PROGRESS_BUCKETS`、`LS_PROGRESS_COEFF` をモデルの設定に合わせて指定してください。
`rescore_psv`、`gensfen`、`benchmark` の外部 USI 経路は指定したオプションをエンジンへ渡せます。
外部 USI 経路のないツールは現時点で Q16 モデルを処理できません。
`progresskpabs` への置き換えは量子化境界で異なる bucket を選ぶため、回避策にはなりません。

## progress 係数の保持と再読込み

係数はロードした値をメモリに保持し、実行中のファイル差し替えでは再読込みしません。
別の係数で処理するときは、対応する係数パスを指定してツールを再起動してください。
native探索はstackの生成・reset時に取得したsnapshotを使い、`gensfen`のbucket集計も
worker開始時の係数を保持します。係数の形式・CLI名・bucket計算式は変わりません。
