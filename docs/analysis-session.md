# メモリ内の局面解析セッション

`rshogi_core::search::AnalysisSession` は通常対局の `Search::go` と別のライフサイクルで探索状態を保持する。
局面と探索設定はセッション作成時に固定し、再開時は追加予算だけを渡す。

```rust,no_run
use rshogi_core::{position::Position, search::{AnalysisBudget, AnalysisSession, Search, SearchInfo}};
let mut position = Position::new();
position.set_hirate();
let mut search = Search::new_with_eval_hash(128, 16);
search.set_num_threads(4);
let mut session = AnalysisSession::new(search, position, 3)?;
let first = session.prepare(AnalysisBudget::Time(10_000))?;
// first を host に公開してから run。別スレッドから first.pause() で停止できる。
let finished = session.run(&first, Some(|info: &SearchInfo| println!("{}", info.depth)))?;
if finished.resumable {
    let next = session.prepare(AnalysisBudget::Time(10_000))?;
    let continued = session.run(&next, None::<fn(&SearchInfo)>)?;
    assert_eq!(continued.total_nodes, finished.total_nodes + continued.result.nodes);
}
# Ok::<(), rshogi_core::search::AnalysisError>(())
```

- `prepare` は実行公開前に呼ぶ。返却後の停止要求を `run` が消すことはない。
- `pause` は要求受付であり、停止完了は `run` の返却で確認する。全 helper が終了してから返る。
- 古いハンドルは新しい実行を停止できない。他セッションのハンドルや消費済み実行も拒否する。
- `Unlimited`、追加ミリ秒の `Time`、全 worker 合計の追加 `Nodes` を扱う。`Depth` は絶対深さによる開発・検証用上限。
- ノード予算は停止チェックごとに各 worker の増分を共有する。チェック間隔と並列停止のため厳密な一致ではなく超過がある。
- 各 worker は自身の完了深さ + 1 から再開する。中断反復の根の結果は前回完了時へ戻し、TT・履歴は保持する。
- 表示・最終結果の基準は main。通常対局の best-thread 選択や最終 info 再出力は解析で使わない。
- 新しい反復が完成しなければ info を再発行しない。前回の結果は `AnalysisFinished.result`、今回／累積の消費は専用フィールドで返す。
- `worker_depths` は論理 ID 順の実行前／後の完了深さ。main は ID 0。デバッグ用に helper の深さの違いを確認できる。
- 評価関数・係数・評価設定の変更はセッションを失効させる。モデル設定と探索の並行実行は禁止（既存グローバル評価状態の排他契約）。
- 設定や対象の変更には新しいセッションを作る。`into_search` は保持状態を手放して探索器を返す。新規セッションは以前の TT・評価キャッシュ・履歴をクリアする。
- native の探索／コールバック panic は helper を停止し `RunFailed` として失効する。Wasm の abort は Rust 側で回復できないため host が Worker 群を破棄する。

## Wasm の停止制御

`AnalysisRunHandle::control_address()` は専用 `AtomicU32` のバイト位置。共有 memory の host は
`Atomics.compareExchange(view, index, runId, runId | 1)` で停止する。
通常の Worker メッセージだけでは同期探索中に停止を処理できない。

アドレスはハンドル生存中のみ有効。解放後にキャッシュした view／アドレスを使ってはならない。
メモリ拡張後には view を作り直す。アプリの session ID と run ID を照合して古い cancel を破棄する。
非共有ビルドに協調停止・resume 対応を表示してはならない。

Wasm helper は物理スレッドの TLS ではなく、論理 worker ID に対応する所有 slot から Box を受け取り、
終了時に返す。探索中に slot の Mutex は保持しない。

ファイルへのチェックポイント保存・復元はこの API に含めない。
