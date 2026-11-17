# rshogi-mcts

`rshogi-core` の盤面と `rshogi-dl::Evaluator` を使う、単一 worker・同期推論の MCTS です。GPU を持たない evaluator でも利用できます。

`Search::new(Config)` で node / edge arena と探索用 buffer を確保します。`Search::run` には局面、開始局面からの `History`、evaluator、時間・simulation 上限、停止フラグを渡します。終了時には、推論エラーの場合も入力局面と履歴を復元します。各 run で木を再構築し、cache・部分木再利用・統計の合流は行いません。

合法手の logits に temperature を適用して正規化し、`Q + cpuct * prior * sqrt(parent visits) / (1 + edge visits)` で選択します。未訪問手の Q は 0.5、同点では合法手生成順を維持します。value は現在の手番の勝率で、親への backup では `1-value` を使います。

訪問数は完了した simulation 数です。根の初回評価も一回として数えるため、根の訪問数と子 edge の合計は同じではありません。NN 評価数は別に返します。最善手は確定勝ちを優先し、確定負けを避け、それ以外は訪問数と prior の順で選びます。訪問数 0 では最初の合法手を返します。root が内部で引き分けや履歴上の勝ちとなっても、合法手があれば fallback を返します。トライルールでは勝ちとなる玉の移動手を返します。

`History` は開始局面以後の全手順で四回同一局面と連続王手を判定します。開始局面以前の手順は復元しません。優等・劣等局面をルール上の終局とはみなしません。詰み・宣言勝ち・引き分けは近似 value と区別し、木の確定結果として伝播します。

arena が上限へ達した場合は、完了した探索結果を返します。最大深さでは既に得た静的 value を使い、総手数の上限では引き分けとします。実行中の backend の forward は強制中断できないため、停止応答は一回の推論時間にも依存します。

## 同期 leaf batch

`Config { batch_size: 16, ..Config::default() }` で葉をまとめる上限を指定できます (1〜1024)。
既定の1、または evaluator が入力形式と `encoded_batch_capacity` の両方を宣言していない場合は、
従来の scalar loop を使います。backend の最大容量が小さければ、その容量までに制限します。
Rust の `Config` を全 field 指定で構築する場合は `batch_size: 1` を加えてください。

batch では一つの局面と履歴を葉ごとに undo / truncate して再利用します。未完了の葉を予約し、
完了訪問数と別の整数 inflight を持ちます。PUCT の Q は完了分の平均だけを使い、探索項の
`sqrt(parent visits + parent inflight)` と `1 + edge visits + edge inflight` で予約を考慮します。
同じ未展開 node を二度予約せず、予約で全枝が塞がった場合は部分 batch を直ちに処理します。
根の初回評価と、終局・深さ上限を見つけたときも部分 batch を処理します。

将来追加する合法手 edge の容量も予約し、全出力の検査が済んでから選択順に展開・backupします。
上限は完了分と予約分の合計で守り、結果の visits には完了分だけを報告します。
停止・時間上限は葉の収集中と forward の直前に確認します。未開始分は予約を解放して破棄し、
開始済み forward は完了後に全出力を検査して反映します。forward 自体は強制中断しません。

`batch_workspace_bytes()` は tree arena と別に確保した入力・出力・path・合法手・予約 metadata の
実 capacity を含む bytes を返します。node / edge の最大容量に応じた inflight 配列も含み、
allocator の管理領域は含みません。最初の batch run で確保して再利用し、入力形式や最大 batch が
変われば作り直します。evaluator 自身の batch/cache buffer は別です。

batch の選択順は scalar と異なり得ます。throughput の改善と探索品質・棋力は別に比較してください。

既定で無効の `lazy-batch-metadata` feature を有効にすると、inflight 配列と node の blocked フラグを
使用中の node と予約済みの将来 edge に応じて段階的に確保します。確保済みの容量は各 run で
再利用され、`batch_workspace_bytes()` は容量が増えたときに更新されます。tree arena の上限は
変わりません。この feature では探索エラー後に batch workspace を破棄するため、次の batch run
で再作成します。停止・時間上限・arena 上限で正常終了した場合は保持します。
