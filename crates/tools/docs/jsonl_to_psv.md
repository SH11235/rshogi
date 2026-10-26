# jsonl_to_psv

JSONL の指し手を PSV 教師局面へ変換する。
[使い方・スコア欠損・破損行の扱い](pack_tools.md#jsonl_to_psv)を参照。

`interrupted` / `censored` は未決として出力から除外し、`Skipped in-progress` に計上する。
旧 CSA の `draw` + reason `interrupted` / `censored` も除外する。
明示的な非 CSA source には旧理由補正を適用しない。
旧 `draw` + `max_moves` は引分と打切りを復元できないため、引分として保持する。

出力が入力と同じ実体（hardlink・symlink を含む）を指す場合は、書き込み前に拒否します。
