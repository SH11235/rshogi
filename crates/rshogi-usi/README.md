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
| `NetworkDelay` | Network delay compensation (ms) | 0 |
| `NetworkDelay2` | Additional delay for uncertain situations | 0 |
| `EvalFile` | NNUE model path | `eval/nn.bin` |
| `SPSAParamsFile` | Search/net SPSA parameter file | `<auto>` |
| `SPSA_NET_*` | LayerStacks net coefficient delta advertised by `--spsa-net-spec` | 0 |

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

core の `uses_large_pages()` / `Search::tt_uses_large_pages()` は Windows の明示確保だけを
表します。Linux/Android のヒント要求の成否は `huge_page_hint_requested()` /
`Search::tt_huge_page_hint_requested()` で確認できます。

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
