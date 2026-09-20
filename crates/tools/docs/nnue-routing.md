# tools の LayerStacks routing 対応

native 評価経路で選べる mode は `progresskpabs`（tatara の f32）と `kingrank9` です。
`progresskpabsq16`（YaneuraOu / BulletOu SFNN）は core / USI エンジンでは対応していますが、
tools の native 経路は整数係数のロードに対応していないため、明示エラーで停止します。
係数指定の有無や bucket 数 1 でも Q16 mode を受理しません。

対象は `preprocess_psv`、`rescore_psv`、`gensfen` の native backend、
`benchmark` の内部 API、`label_bench_positions`、`rescore_hcpe`、`yardstick_label`、
`bench_nnue_eval`、`verify_nnue_accumulator` です。

Q16 モデルには対応する USI エンジンを使い、`LS_BUCKET_MODE=progresskpabsq16`、
`LS_PROGRESS_BUCKETS`、`LS_PROGRESS_COEFF` をモデルの設定に合わせて指定してください。
`rescore_psv`、`gensfen`、`benchmark` の外部 USI 経路は指定したオプションをエンジンへ渡せます。
外部 USI 経路のないツールは現時点で Q16 モデルを処理できません。
`progresskpabs` への置き換えは量子化境界で異なる bucket を選ぶため、回避策にはなりません。
