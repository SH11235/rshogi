# verify_nnue_accumulator

LayerStacks の accumulator 更新を refresh と照合します。
使用可能な引数は `verify_nnue_accumulator --help` を参照してください。

`--ls-bucket-mode` は `progresskpabs` または `kingrank9` に対応します。
`progresskpabsq16` は整数係数ロードが未対応のため明示エラーになります。
詳細は [routing 対応表](nnue-routing.md) を参照してください。

モデルの読み込みには静的 LayerStacks 専用の reader を使います。同じビルドの他 crate
経由で universal edition の `nnue-runtime-dimensions` が feature 統合されていても、
LayerStacks net を静的構造のまま読み込めます（CLI は変わりません）。
