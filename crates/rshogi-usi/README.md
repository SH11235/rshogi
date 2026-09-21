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

## License

GPL-3.0-or-later License

## 参考・影響 / Acknowledgements

本プロジェクトは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) およびチェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。
アルゴリズムや評価のアイデアに影響を受けていますが、実装と構成は独自です。
