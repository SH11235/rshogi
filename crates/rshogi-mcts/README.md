# rshogi-mcts

`rshogi-core` の盤面と `rshogi-dl::Evaluator` を使う、単一 worker・同期推論の MCTS です。GPU を持たない evaluator でも利用できます。

`Search::new(Config)` で node / edge arena と探索用 buffer を確保します。`Search::run` には局面、開始局面からの `History`、evaluator、時間・simulation 上限、停止フラグを渡します。終了時には、推論エラーの場合も入力局面と履歴を復元します。各 run で木を再構築し、cache・部分木再利用・統計の合流は行いません。

合法手の logits に temperature を適用して正規化し、`Q + cpuct * prior * sqrt(parent visits) / (1 + edge visits)` で選択します。未訪問手の Q は 0.5、同点では合法手生成順を維持します。value は現在の手番の勝率で、親への backup では `1-value` を使います。

訪問数は完了した simulation 数です。根の初回評価も一回として数えるため、根の訪問数と子 edge の合計は同じではありません。NN 評価数は別に返します。最善手は確定勝ちを優先し、確定負けを避け、それ以外は訪問数と prior の順で選びます。訪問数 0 では最初の合法手を返します。root が内部で引き分けや履歴上の勝ちとなっても、合法手があれば fallback を返します。トライルールでは勝ちとなる玉の移動手を返します。

`History` は開始局面以後の全手順で四回同一局面と連続王手を判定します。開始局面以前の手順は復元しません。優等・劣等局面をルール上の終局とはみなしません。詰み・宣言勝ち・引き分けは近似 value と区別し、木の確定結果として伝播します。

arena が上限へ達した場合は、完了した探索結果を返します。最大深さでは既に得た静的 value を使い、総手数の上限では引き分けとします。実行中の backend の forward は強制中断できないため、停止応答は一回の推論時間にも依存します。
