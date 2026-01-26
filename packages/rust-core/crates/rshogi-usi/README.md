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

## License

MIT License
