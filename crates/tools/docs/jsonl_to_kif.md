# jsonl_to_kif

JSONL 対局ログを KIF 棋譜に変換する。

```bash
jsonl_to_kif --input games.jsonl --output kif_out/
```

出力先がディレクトリなら `g001.kif` 等を作成する。`--game-id 1,5` で対局IDを、
`--skip 10 --limit 10` で順序による範囲を選べる。両方式は併用できない。
指し手のないログは開始局面を復元できないため変換できない。

`black_win` / `white_win` は先手勝ち / 後手勝ち、`draw` は引分となる。
`interrupted` / `censored`、未知の outcome、result のない進行中棋譜は中断と表示する。
旧 CSA の `draw` + reason `interrupted` / `censored` も中断とする。
明示的な source がない場合だけ旧 CSA 識別文を使い、非 CSA の理由補正はしない。
旧 `draw` + `max_moves` は引分と打切りを復元できないため引分として保持する。
