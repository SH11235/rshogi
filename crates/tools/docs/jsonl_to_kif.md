# jsonl_to_kif

JSONL 対局ログを KIF 棋譜に変換する。

```bash
cargo run -p tools --release --bin jsonl_to_kif -- \
  --input games.jsonl --output kif_out/
```

出力先がディレクトリなら `g001.kif` 等を作成する。`--game-id 1,5` で対局IDを、
`--skip 10 --limit 10` で順序による範囲を選べる。両方式は併用できない。
指し手のないログは開始局面を復元できないため変換できない。

`black_win` / `white_win` は先手勝ち / 後手勝ち、`draw` は引分となる。
`interrupted` / `censored`、未知の outcome、result のない進行中棋譜は中断と表示する。
旧 CSA の `draw` + reason `interrupted` / `censored` も中断とする。
明示的な source がない場合だけ旧 CSA 識別文を使い、非 CSA の理由補正はしない。
旧 `draw` + `max_moves` は引分と打切りを復元できないため引分として保持する。

先後の対局者名は各局の move 行の engine 名から決める。対応する meta ラベルがある場合は
エンジン名と USI オプションを併記する。ラベルの照合を優先し、ラベルが一致しない場合だけ
バイナリ名が片側のみに一致するときに補完する。move 行に engine 名がない旧ログは meta を使い、
`pair_slot=1` の先後交換を反映する。投了・宣言勝ち・時間切れ・不正手・着手なしの
終局応答と `timed_out=true` の着手は盤上の指し手に変換せず、result の勝敗と終了理由を記載する。

出力と入力の同一パス・hardlink、出力自体の symlink は拒否する。
KIF は 1 局ずつ一時ファイルで完成させてから置き換えるため、変換に失敗した局の既存出力は保持する。
複数局の変換途中で失敗した場合、それ以前に完成した局の出力は残る。実行中に別プロセスから
入力・出力のパスを差し替えないこと。

持ち時間と加算時間は先後別に出力する。旧ログで `settings.wtime` / `settings.winc` が
欠けている場合は、それぞれ `settings.btime` / `settings.binc` で補完する。後手側に
明示された `0` は保持する。先後いずれかに正の加算時間があれば、加算時間の行を記載する。
