# USIトークンの厳密解析と互換性

## 判断

`Move::from_usi` と `Square::from_usi` の既存の末尾許容を維持し、トークン全体を検証する `from_usi_strict` を追加する。strict replayとJSONのcell.squareだけを厳密版へ移す。

通常手は4文字、成りは末尾の `+` を含む5文字、打ちは4文字、座標は2文字を要求する。Moveの `none` / `pass` / `0000` と `win` 拒否は既存どおり。これは字句検査であり、局面での合法性検査は別に行う。

## 呼出側の調査

調査基点は `0d4c3b644`。`crates` 以下のRustソースで `Move::from_usi` / `Square::from_usi` の直接参照を棚卸しし、実行経路とテストを区別した。

| 呼出側 | 入力と扱い |
|---|---|
| core `position/json_conversion.rs` | replayは棋譜の1手文字列、JSONは1升の座標文字列。厳密版へ移行する |
| USI `main.rs` | position/searchmovesで分割済みトークンを渡す。今回の互換変更には含めない |
| tools `selfplay/position.rs`, `game.rs`, `backend.rs` | 開始棋譜、bestmove、MultiPVの先頭手を解析。game.rsには生の応答から余分な末尾を持ち越さず正規化する旨のコメントがある |
| tools `kif.rs`, `jsonl_to_psv.rs`, `replay/jsonl_source.rs` | JSONLのmove_usiフィールド。後続の合法性検査や終局ラベル処理へ進む |
| tools `book_backprop`, `book_extend`, `book_rescore`、book `probe.rs` | 定跡の手・ponderフィールド。解析後に局面との整合性を検査する |
| tools `book_from_csa`, `extract_bench_positions`, `replay/csa_source.rs` | CSAから生成したUSI文字列を解析する |
| tools `gensfen.rs` | 解析済みbest_move、または生のbestmove文字列からのフォールバックを使う |
| core Move内部のSquare解析 | 2文字ずつ切り出して渡す。既存処理を維持する |
| その他の直接参照 | 固定USIや手列を用いるテスト。意図的な末尾許容を要求するテストは確認できなかった |

`from_usi` はpublic APIであり、リポジトリ外の利用者の契約を網羅できていない。selfplayの正規化コメントだけから任意の不正入力の受理を保証するものではないが、既存API全体を厳密化する根拠にもならないため、入口を分離した。

## 検証と範囲

厳密版は正常な手・特殊値と81升の座標を受理し、合法prefixに続く文字、余分な `+`、打ちへの `+`、空白・改行・NUL・非ASCIIの余分な文字を拒否する。既存APIのprefix解析も回帰で保持する。

strict replayの字句不正は従来の解析不能入力と同じ `Result::Err` を返す。JSONの不正座標も復元時のエラーになる。通常USI、自己対局、既存定跡・ログの呼出側は移行していない。既存データの末尾付き入力の件数や実対局への影響は未測定。