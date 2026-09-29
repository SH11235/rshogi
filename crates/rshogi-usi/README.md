# rshogi-usi

USI (Universal Shogi Interface) protocol implementation for the rshogi engine.

## Features

- **Full USI protocol support** - Compatible with GUI applications like ShogiGUI, Shogidokoro, etc.
- **NNUE evaluation** - High-quality position evaluation using neural networks
- **Configurable options** - Thread count, hash size, time management parameters
- **Cross-platform** - Works on Windows, macOS, and Linux

## Installation

```bash
cargo install rshogi-usi
```

Or build from source:

```bash
cargo build --release -p rshogi-usi
```

## Usage

Run the engine directly:

```bash
rshogi-usi
```

The engine will start in USI mode, waiting for commands from stdin.

### USI Options

| Option | Description | Default |
|--------|-------------|---------|
| `Threads` | Number of search threads | 1 |
| `USI_Hash` | Hash table size in MB | 256 |
| `EvalHash` | Evaluation hash size (MiB, 0 for an empty table) | 256 |
| `UseEvalHash` | Use the evaluation hash | true |
| `EvalHashLargePages` | Attempt Large Pages allocation for EvalHash (false uses a regular Vec allocation) | true |
| `NetworkDelay` | Network delay compensation (ms) | 0 |
| `NetworkDelay2` | Additional delay for uncertain situations | 0 |
| `EvalFile` | NNUE model path | `eval/nn.bin` |
| `SPSAParamsFile` | Search/net SPSA parameter file | `<auto>` |
| `SPSA_NET_*` | LayerStacks net coefficient delta advertised by `--spsa-net-spec` | 0 |

`PassRights` / `InitialPassCount` / `PassMoveBonus` / `PassRightValueEarly` / `PassRightValueLate`
configure the optional finite pass-rights rule. The position and move generation support it in
every build, and `PassMoveBonus` always applies. The search adds the pass-rights term
(`PassRightValueEarly` / `PassRightValueLate`) and uses PASS in null-move pruning only when the
engine is built with the `search-pass-rules` feature (off by default, also in `cargo xtask build`
presets). Without it the engine prints a notice when `PassRights` is turned on.

`SPSA_NET_*` options are loaded from the model again and applied at the next `isready`,
`usinewgame`, or `go`; changing several options therefore causes one reload.

## Allocation diagnostics

Build with `cargo build --profile production -p rshogi-usi --features allocation-stats`
(select your usual edition features when using a specialized engine). Each actual
search prints five `info string allocation_events` lines before any `bestmove` output
(also when a replacement `go` suppresses the previous search's `bestmove`). The feature
is disabled by default. The optional `tracking-allocator` dependency provides a
safe callback API over `System`. Its wrapper adds allocation metadata and handles
reallocation by allocating, copying and freeing. Use the normal build for timing:
the diagnostic build changes allocator behavior as well as adding counter overhead.

The reported interval starts immediately before `Search::go` and ends after it
returns, including helper searches. It excludes model loading, `position`, thread
creation in the USI layer, diagnostic printing, and `bestmove` formatting. Book
hits do not enter this interval and produce no allocation report.

| Phase | Allocation call sites |
|-------|-----------------------|
| `Setup` | Main thread `Search::go` outside the nested scopes, including final result assembly |
| `Iteration` | Main/helper iterative deepening, root list initialization and iteration bookkeeping |
| `Tree` | Main/helper root search and its descendants, including root/PV bookkeeping |
| `Output` | Public PV validation/copy and USI info callback, plus final info assembly |
| `Other` | Unscoped work anywhere in the process during the interval, including helper preparation and concurrent protocol input |

`allocated` and `deallocated` count callback events, not individual `System` API
operations. Zeroed allocation produces one allocation event; reallocation produces
an allocation event for the full new size and a deallocation event. `object_bytes`
sums requested object sizes, excluding wrapper metadata, not live memory or net
growth. These events are not directly comparable to direct allocator-operation
counts. Tracking starts at program entry; frees for allocations made before
tracking was enabled are not reported. Classification uses the phase of each call, so a free may
occur in a different phase from its allocation. Counters are process wide and
helper threads set their own scopes; concurrent searches in one process would
overlap. Native OS allocations bypassing Rust's global allocator are not counted.
The core feature only supplies scopes/counters: a library consumer must install
its own recording allocator. Compare fixed-depth searches with the same model,
options and thread count, and retain cold and warm searches separately.

## TT の page 配置表示

Windows で `MEM_LARGE_PAGES` による確保に成功した場合は `Large Pages are used.` を
表示します。Linux/Android では `MADV_HUGEPAGE` 要求に成功した場合だけ
`Huge-page hint requested; actual page backing is managed by the OS.` を表示します。
ヒント要求は実際の huge-page backing を保証しません。要求失敗時や通常 page への
フォールバック時は、large-page 使用を示す行を表示しません。

表示は TT を取り直す操作（`isready`、`usinewgame`、`USI_Hash` の変更、`SPSA_NET_*` の再読み込み）の
後に確認し、直前に表示した配置から変わったときだけ出します。
TT を取り直した結果 large pages やヒントが使えなくなった場合は
`The TT now uses regular pages.` を表示します。

core の `uses_large_pages()` / `Search::tt_uses_large_pages()` は Windows の明示確保だけを
表します。Linux/Android のヒント要求の成否は `huge_page_hint_requested()` /
`Search::tt_huge_page_hint_requested()` で確認できます。

EvalHash も同じ確保処理を使い、Windows で成功すると `EvalHash: Large Pages are used.` を
TT と同じ `info string` 内の JSON メッセージ形式で表示します。Linux/Android は
`EvalHash: Huge-page hint requested; actual page backing is managed by the OS.` です。
権限が無い場合や Large Pages 用メモリが足りない場合は通常ページへフォールバックします。
配置が通常ページに戻ったときは `EvalHash: Regular pages are used.` と表示します。
`EvalHash=0` では表が空のため、この通常ページの表示は出しません。

`EvalHashLargePages=false` と `true` は同じバイナリで切り替えられます。
変更時は現在確保済みのサイズでキャッシュを再確保し、サイズ変更後も設定を保持します。
初期状態の EvalHash は最初の `go` 直前まで遅延確保され、配置表示も確保後に行います。
Windows の Large Pages は非ページングメモリを使います。既定サイズでは、各プロセスにつき
TT 256 MiB と EvalHash 256 MiB、定常時は計 512 MiB の確保を試みます。
TT の取り直しでは旧 TT の解放前に新 TT を確保するため、瞬間最大は旧 TT + 新 TT + EvalHash
（既定サイズでは 768 MiB）です。複数プロセスではそれぞれの合計容量が必要です。
EvalHash の取り直しでは旧表を先に手放してから新表を確保します。
ライブラリの `EvalHash::new` / `Search::new` / `Search::new_with_eval_hash` は通常ページが既定で、
Large Pages は明示指定時だけ要求します。`rescore_psv` などのツールの既定動作は変わりません。

## 兄弟局面の先読み（計測用）

`setoption name TtSiblingPrefetch value true` で、通常探索の次の兄弟局面の
TT と、有効なら EvalHash を先読みします。既定は `false` で、`usi` の option
一覧には表示しません。各 worker が探索開始時に設定を取り込みます。

対象は MovePicker の同じ段階で順序が確定した範囲です。段階の遷移、未ソートの
quiet、SEE による選別前の GoodCapture / ProbCut、qsearch は対象外です。
先読みした手が枝刈りされる場合もあります。高速化の効果は別途計測が必要です。

## mimalloc (`mimalloc`)

既定で無効の `mimalloc` は、エンジン binary の global allocator を
[mimalloc](https://github.com/microsoft/mimalloc) に置き換えます。
`libmimalloc-sys` が C のソースをビルドするため、有効にするには C コンパイラが必要です。
`mimalloc` 0.1.52 / `libmimalloc-sys` 0.1.49 は既定で mimalloc v3 系 (3.3.2) をビルドします。

```bash
# preset edition に mimalloc を追加して build
# → engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-none+mimalloc に配置
cargo xtask build --edition layerstacks-halfka_hm_merged-1536x16x32-none --features mimalloc
```

`--features` の仕様と binary の命名規則は [`docs/build.md`](../../docs/build.md) を参照してください。
xtask を使わない場合は cargo に直接指定します。

```bash
cargo build --profile production -p rshogi-usi --no-default-features \
  --features edition-layerstacks-halfka_hm_merged-1536x16x32-none,mimalloc
```

固定 depth の探索（MultiPV 1 と 3）では、既定 build と nodes / score / bestmove / PV が
一致することを確認しています。時間制限つきの探索は、速度差の分だけ到達 depth が変わりえます。

`allocation-stats` も global allocator を設定するため、同時には指定できません。
library (`rshogi-core`) の allocator は変えません。

Windows / Ryzen 9 9950X3D2 で、同一 binary のまま allocator だけを切り替えた 1 thread の
探索区間の比較（上記の版、4 局面 × 5 秒 × 64 探索を 4 回）では、NPS が +0.20〜+0.42%、
cycles/node が −0.19〜−0.36% でした。複数 thread と他の OS では計測していません。

## License

GPL-3.0-or-later License

## 参考・影響 / Acknowledgements

本プロジェクトは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) およびチェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。
アルゴリズムや評価のアイデアに影響を受けていますが、実装と構成は独自です。

### TT書き込み診断 (`tt-write-stats`)

`--features tt-write-stats` を付けた診断ビルドは、実際に探索した `go` の終了時に1行の
`info string tt_write_events` を出力します。定跡ヒットで探索を省略した場合は出力しません。
通常ビルドでは無効です。
共有TTを使う全workerの `ProbeResult::write` の探索前後差分を報告します。

- `attempts`: 格納操作数。書き込み直前に再loadしたslotを分類します。
- `empty` / `same_key16` / `different_key16`: 空、使用中で短縮キー一致、不一致。合計はattemptsです。
- `payload_accepted` / `payload_retained`: move以外のkey/depth/gen/bound/value/evalの採用・保持。
  合計はattemptsです。同じ値を再採用した場合もacceptedに含みます。
- `same_key16_same_depth_accepted`: 使用中slotで短縮キーと深さが同じ採用。
- `move_only_changed` / `unchanged`: payload保持時にmoveが変わった・変わらなかった数。
  合計はpayload_retainedです。

短縮キーは16bitのためfull key一致や衝突は判別できません。snapshotは複数項目を同時には
読みません。core APIではwriter停止後に読み、同じTTのsnapshot同士を `since` で比較してください。
`clear` / `resize` は診断の累積値をリセットしません。別のTTの値とは差分を取れません。
並行writerに上書きされたか、実際に最後まで保持されたentry数は分かりません。
浅い保存でも世代・Exact・PV条件で採用されるため、retainedを単純な「浅いentry棄却」とは扱いません。
計数はTTごとの固定サイズatomic配列を使い、共有書き込み時の競合・追加コストがあります。
速度比較にはこのfeatureを無効にした通常ビルドを使ってください。
