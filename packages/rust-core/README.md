# Rust Core for Shogi

[![codecov](https://codecov.io/gh/SH11235/shogi/branch/main/graph/badge.svg?flag=rust-core)](https://codecov.io/gh/SH11235/shogi)

This workspace contains the Rust core engine and WebAssembly (WASM) modules for advanced Shogi features, including WebRTC communication, mate search, and opening book functionality.

## Features

- 🌐 WebRTC peer-to-peer communication for online play
- 🔍 Mate search algorithm implementation
- 📚 Opening book with binary format support
- 🎯 High-performance position hashing and move encoding
- 🤖 USI protocol engine with multiple search/evaluation modes
- 🧠 NNUE evaluation function support
- ⚡ Enhanced search with advanced pruning techniques
- 📊 NNUE training tools for machine learning

## Prerequisites

- Rust toolchain (install from https://rustup.rs/)
- wasm-pack (`cargo install wasm-pack`) — only required for WASM builds
- cargo-tarpaulin (optional, for coverage reports): `cargo install cargo-tarpaulin`

## Project Structure

```
crates/
├── engine-core/             # Core engine implementation
│   ├── search/             # Search algorithms (basic & enhanced)
│   ├── evaluation/         # Evaluation functions (material & NNUE)
│   └── time_management/    # Time control
├── engine-usi/              # USI protocol command-line interface
└── webrtc-p2p/             # WebRTC communication

src/                         # Legacy WASM modules
├── lib.rs                   # Main library entry point
├── simple_webrtc.rs        # WebRTC implementation
├── mate_search.rs          # Mate search algorithm
├── opening_book/           # Opening book module
│   ├── mod.rs             # Module exports
│   ├── binary_converter.rs # Binary format conversion
│   ├── data_structures.rs  # Core data types
│   ├── move_encoder.rs     # Move encoding/decoding
│   ├── position_filter.rs  # Position filtering logic
│   ├── position_hasher.rs  # Position hashing
│   └── sfen_parser.rs      # SFEN format parsing
└── opening_book_reader.rs  # Opening book reader interface
```

## Documentation

- [Engine Types Guide](docs/engine-types-guide.md) - エンジンタイプの選択ガイド（推奨: EnhancedNnue）
- [NNUE Evaluation Guide](docs/nnue-evaluation-guide.md) - NNUEモデルの性能評価方法
- [Performance Documentation](docs/performance/) - ベンチマーク、プロファイリング、性能分析
- [Development Guide](docs/development/) - TDD開発ガイド、テスト戦略
- [Implementation Docs](docs/implementation/) - 実装詳細
- [Reference](docs/reference/) - フォーマット仕様など
- [Distillation: Teacher Value Domain](docs/distillation/teacher_value_domain.md) - 教師値ドメインと Classic 蒸留ガイド

## USI Engine Usage

### Quick Start
```bash
# Build and run the USI engine
cargo build --release --bin engine-usi
./target/release/engine-usi

# Set to strongest mode (EnhancedNnue)
setoption name EngineType value EnhancedNnue

# Basic commands
usi
isready
position startpos
go movetime 1000
quit
```

### Performance Build & Features

- 推奨ビルド（最適化）
  - `RUSTFLAGS="-C target-cpu=native" cargo run -p engine-usi --release`
- フィーチャー（engine-usi から engine-core へ伝播）
  - 注: `nnue_single_diff`（SINGLE 差分NNUE）は恒久化され常時有効です
  - 任意ON:
    - `fast-fma`: FMAで出力加算を高速化（丸め微差を許容できる場合）
    - `diff-agg-hash`: 差分集計をHashMap実装でA/B（大N向け）
    - `nnue-telemetry`: 軽量テレメトリ（探索中の経路割合など）
    - `tt_metrics`, `ybwc`, `nightly`: 必要に応じて
    - `diagnostics`（メタ）: 下記の診断系を一括ON
      - `engine-core/tt_metrics`（TT詳細メトリクス）
      - `engine-core/nnue_telemetry`（NNUE経路テレメトリ）
      - `engine-core/pv_debug_logs`（PVデバッグ出力: stderr; 実行時環境変数は不要）

例: 診断系を一括ON（配布バイナリで挙動固定）
```bash
cargo run -p engine-usi --release --features diagnostics
```

例: 差分NNUE + FMA 有効
```bash
RUSTFLAGS="-C target-cpu=native" \
cargo run -p engine-usi --release --features fast-fma
```

注: fp32 行加算用 SIMD は Dispatcher に統合済みで常時ON（実行時 CPU 検出: AVX/FMA/SSE2/NEON/Scalar）。`simd` フィーチャは不要です。

起動時に `info string core_features=engine-core:...` を出力します（再現性・ログ用途）。

### Panic ハンドリング方針（engine-usi は panic=unwind 前提）

- 本エンジンの USI バイナリ（`engine-usi`）は、異常時にプロセスを落とさず復旧するため、Rust の `panic = "unwind"` を前提としています。
  - `Cargo.toml`（workspace の `[profile.dev]` / `[profile.release]`）で `panic = "unwind"` を明示済み。
  - これにより、`go`/`position`/`setoption` 等のハンドラ内部で発生したパニックは `catch_unwind` により捕捉され、ログ出力とフォールバック経路（必要に応じて `bestmove`）で継続します。
- もし配布ポリシー等で `panic = "abort"` を使用する場合、この安全化は無効化されます。対局用途では `unwind` を強く推奨します。

運用ログ（例）:

```
info string go_dispatch_enter
info string go_enter cmd=go btime 0 wtime 0 byoyomi 10000
info string go_panic_caught=1
info string fallback_bestmove_emit=1 reason=go_panic move=... sid=... root=...
bestmove ...
```

### USI出力（診断強化）
- 探索中の`info`行に`hashfull <permille>`を常時付与します。
- 終局時（finalize/stop）に、MultiPV未使用でも`info multipv 1 ... hashfull ... pv ...`を必ず1本出力します（SinglePVの可視化）。
- `tt_metrics`有効時は、終局直前にTTメトリクスの要約を`info string tt_metrics ...`（複数行）で出力します。
- `pv_debug_logs`はビルド時のfeatureで固定され、PVデバッグ出力（stderr）は配布物ごとにON/OFFが決まります（従来の`SHOGI_DEBUG_PV`環境変数は不要）。

### Engine Types
- **EnhancedNnue** (推奨): 最強 - 高度な探索 + NNUE評価
- **Nnue**: 高速分析用
- **Enhanced**: 省メモリ環境用
- **Material**: デバッグ用

### Engine Options

| Option | Type | Default | Range | Description |
|--------|------|---------|-------|-------------|
| USI_Hash | Spin | 1024 | 1-1024 | Hash table size in MB |
| Threads | Spin | 1 | 1-256 | Number of search threads |
| USI_Ponder | Check | true | true/false | Enable pondering (thinking on opponent's time) |
| EngineType | Combo | Material | Material/Nnue/Enhanced/EnhancedNnue | Engine evaluation and search type |
| ByoyomiPeriods | Spin | 1 | 1-10 or 'default' | Number of byoyomi periods (USI_ByoyomiPeriods alias also supported) |

> Note: `ByoyomiPeriods` accepts the literal `default` to reset to the initial value (the engine handles this as a special case).

#### ByoyomiPeriods オプション

秒読みの回数（period数）を制御します。`USI_ByoyomiPeriods` はエイリアスとして同じ意味で利用できます。`value default` を指定すると初期値（1）に戻ります。

例:

```bash
# デフォルト回数（goでperiods未指定のときに使われる）
setoption name ByoyomiPeriods value 3
# エイリアス（同等）
setoption name USI_ByoyomiPeriods value 3

# 既定（1）に戻す
setoption name ByoyomiPeriods value default

# goコマンド側で上書き
go byoyomi 30000 periods 5  # 30秒×5回
```

### InstantMateMove（短手数詰みの即時確定）

詰みが「確定」したときに、探索を待たず即座にbestmoveを返す機能です。誤発火（Partial/浅深度の暫定PVによる即指し）を防ぐため、ゲートと軽検証を追加しています。

- 代表オプション（既定値）
  - `InstantMateMove.Enabled`（true）: 機能の有効/無効。疑義のある環境では false 推奨。
  - `InstantMateMove.MaxDistance`（1）: 「詰みまでの手数」しきい値（plies）。1=1手詰め相当のみ即確定。
  - `InstantMateMove.CheckAllPV`（true）: MultiPV全行の詰みを確認（falseでPV1のみ）。
  - `InstantMateMove.RequiredSnapshot`（Stable）: Stableスナップショットのみで発火（Partialは不発）。
  - `InstantMateMove.MinDepth`（0）: 追加の深さゲート。0で無効（YaneuraOu流: 証明重視）。
  - `InstantMateMove.VerifyMode`（CheckOnly）: 軽検証モード。
    - Off: 検証なし
    - CheckOnly: 候補手を仮指し→相手合法手が0なら確定
    - QSearch: 将来の軽qsearch用フック（現状はCheckOnly相当）
  - `InstantMateMove.VerifyNodes`（0）: 軽qsearch用の上限ノード（将来使用）。
  - `InstantMateMove.RespectMinThinkMs`（true）: 最小思考時間の尊重を有効化。
  - `InstantMateMove.MinRespectMs`（8）: fast finalize 前に最低限使う思考時間（ms）。

- 運用の勘所
  - まず安全に止める: `setoption name InstantMateMove.Enabled value false`
- 代替として誤検知を減らす: `InstantMateMove.CheckAllPV value true`（既定でtrue）
  - 既定は「Stable限定＋軽検証（CheckOnly）＋最小思考時間8ms尊重」で、Partial・浅深度での誤発火を抑止します。

例: 既定強化（明示）

```bash
setoption name InstantMateMove.Enabled value true
setoption name InstantMateMove.RequiredSnapshot value Stable
setoption name InstantMateMove.CheckAllPV value true
setoption name InstantMateMove.VerifyMode value CheckOnly
setoption name InstantMateMove.RespectMinThinkMs value true
setoption name InstantMateMove.MinRespectMs value 8
```

例: 一時的に完全無効化

```bash
setoption name InstantMateMove.Enabled value false
```

### Threads連動の自動既定（T8/T1 プロファイル）

エンジンは `Threads` を見て、対局安全寄りの既定（プロファイル）を自動で適用します。GUI から明示の `setoption` があればそれを最優先し、自動既定は上書きしません。

- 適用条件（Profile.Mode=Auto の既定）
  - `Threads ≥ 4` → T8 プロファイル
  - `Threads = 1` → T1 プロファイル

- 既定値（要点のみ）
  - T8（Threads≥4）
    - RootSeeGate=On（XSEE=100）
    - PostVerify=On（YDrop=250）
    - Finalize: SwitchMargin=30 / OppSEE_Min=100 / BudgetMs=8
    - MultiPV=1
  - T1（Threads=1）
    - RootSeeGate=On（XSEE=100）
    - PostVerify=On（YDrop=225）
    - Finalize: SwitchMargin=35 / OppSEE_Min=120 / BudgetMs=4
    - MultiPV=1

- ログ（探索開始時）

```text
info string effective_profile mode=Auto resolved=T8 threads=8 multipv=1 \
  root_see_gate=1 xsee=100 post_verify=1 ydrop=250 \
  finalize_enabled=1 finalize_switch=30 finalize_oppsee=100 finalize_budget=8 \
  overrides=- threads_overridden=0
```

メモ:
- `effective_profile` は「最終的に有効な設定」を1行で可視化します。GUIの `setoption` で上書きされたキーは `overrides` に列挙されます。
- `Profile.Mode` を `T1`/`T8`/`Off` に切り替えることで、自動既定を明示固定または無効化できます。

## Building

### From project root
```bash
npm run build:wasm      # Production build (optimized)
npm run build:wasm:dev  # Development build (faster)
```


## Important Notes

⚠️ **WASM files must be built before running the web application!**

The build process:
1. Compiles Rust code to WebAssembly
2. Generates JavaScript bindings and TypeScript definitions
3. Copies the generated files to `packages/web/src/wasm/` (when using the web frontend in this monorepo)

The generated files in `packages/web/src/wasm/` are:
- Excluded from git (in .gitignore)
- Required for the web application to run
- Must be regenerated when Rust code changes

## Development Workflow

1. Make changes to Rust code
2. Run quality checks: `cargo fmt`, `cargo clippy`, `cargo test`

## Testing

```bash
# Run standard Rust tests
cargo test

# Run WASM tests in browser (requires Chrome)
wasm-pack test --chrome --headless

# Generate code coverage report (requires cargo-tarpaulin)
cargo tarpaulin --out html --lib  # Generates tarpaulin-report.html
cargo tarpaulin --out Xml  # Generates cobertura.xml for CI

# Benchmark tests (ignored by default due to execution time)
cargo test -- --ignored              # Run only ignored tests (benchmarks)
cargo test -- --include-ignored      # Run all tests including benchmarks
cargo test test_benchmark -- --ignored  # Run specific benchmark test
```

### Criterion Benches

Run the always-on SINGLE NNUE chain benchmark:

```bash
cargo bench -p engine-core --bench nnue_single_chain_bench -- nnue_single_chain
```

Reports are generated under:

```
target/criterion/nnue_single_chain/*/report/index.html
```

Open the latest report in your browser (example on macOS):

```
open target/criterion/nnue_single_chain/*/report/index.html
```

Tips for reproducible results:

- Pin CPU cores (e.g., `taskset -c 0` on Linux)
- Keep the system idle during runs
- Consider disabling turbo/CPU frequency scaling during measurement

## Parallel Bench Notes (LazySMP)

- BenchAllRun（全スレッド全力実行）
  - 環境: `SHOGI_PAR_BENCH_ALLRUN=1` を指定すると、Primary 完了後も Helper を最後まで待ち合わせます。
  - ログ: `info string helpers_join_ms=... received=X/Y canceled=0|1` を1回出力します。
    - `received` は受信できた Helper 件数（Y=Threads-1）
    - `canceled=1` はベンチの安全装置が働き、期限超過でフォールバックしたことを示します。
  - 期限の決定順: `SHOGI_PAR_BENCH_JOIN_TIMEOUT_MS` > TimeManager(hard/soft) > FixedTime+1000ms > 既定3000ms

- 通常対局（BenchAllRun=0）
  - `stop_flag` による自発停止＋短時間ドレインで即応性を重視します。
  - ドレインの総時間は `SHOGI_STOP_DRAIN_MS`（既定45ms）で制御できます（0で無効）。
  - 旧挙動（Primary直後にHelperをキャンセル）を比較したい場合は `SHOGI_PAR_CANCEL_ON_PRIMARY=1` を設定します。

- qsearch ノード上限のセンチネル
  - `qnodes_limit(0)` を指定すると **無制限**（センチネル）として扱われます。
  - デフォルト上限（`DEFAULT_QNODES_LIMIT=300,000`）の影響を避けたいベンチでは `0` を明示してください。


## Code Quality

### Required Checks (run automatically on pre-commit)
```bash
cargo fmt                    # Format code
cargo clippy -- -D warnings  # Lint with warnings as errors
cargo check                  # Fast type checking
```

### Additional Tools
```bash
cargo audit      # Security vulnerability scan
cargo outdated   # Check for outdated dependencies
cargo machete    # Find unused dependencies (requires installation)
```

## API Documentation

### WebRTC Module
Provides simple WebRTC functionality for peer-to-peer connections:
- Connection establishment
- Message passing
- Error handling

### Mate Search Module
Implements efficient mate search algorithms:
- Depth-limited search
- Move ordering optimization
- Performance-oriented design

### Opening Book Module
Handles opening book data in binary format:
- **Binary Format**: Compact storage of positions and moves
- **Position Hashing**: Fast lookup using FNV-1a algorithm
- **Move Encoding**: Efficient 16-bit move representation
- **SFEN Support**: Parse and convert SFEN notation
- **Database**: Currently supports 100,000+ opening positions

### NNUE Training Tools
Machine learning tools for NNUE evaluation function:
- **train_wdl_baseline**: Lightweight WDL (Win/Draw/Loss) trainer for pipeline validation
- **train_nnue**: Full NNUE trainer with HalfKP features and row-sparse updates
  - Performance metrics: loader_ratio and examples/sec monitoring
  - Cache support for faster data loading
  - Minimal training dashboard: per-epoch metrics, phase metrics, calibration (CP-binned ECE)
  - Deterministic runs: specify `--rng-seed <u64>` (`--seed` is kept as an alias)
  - Classic export: combine `--export-format classic-v1` with `--emit-fp32-also` to emit `nn.classic.nnue`, `nn.fp32.bin`, and `nn.classic.scales.json`

See tools README for usage, options, and outputs:
- crates/tools/README.md (Minimal Training Dashboard: baseline and NNUE)

#### 手動ベンチ（GitHub Actions）: NNUE Stream Loader Bench
- 目的: stream-cache ローダとプリフェッチの効果検証（sps / loader_ratio を比較）。
- 実行: GitHub Actions → 「NNUE Stream Loader Bench (manual)」→ Run workflow。
- 仕様: 小規模データを合成し、prefetch=0（同期）/8（非同期）で 1 epoch 実行。ジョブサマリに sps と loader_ratio を出力。
- 備考: デフォルトで gzip を使用（zstd 機能は不要）。しきい値による自動失敗は未設定（必要なら追加）。
 - 入力は JSONL / Cache を自動判定（Cache は NNFC マジックヘッダで検出）

例: ストリーミング学習（事前ロードなし）
```bash
cargo run -p tools --bin train_nnue -- \
  -i runs/data.cache.gz -e 1 -b 16384 \
  --stream-cache --prefetch-batches 8 --throughput-interval 2.0
# ログ: [throughput] mode=stream ... sps=... loader_ratio=...%
```

補足:
- `loader_ratio` は、ストリーミング時の「ローダ待ち（I/O/解凍/受信待機）」が占める比率です。
- 事前メモリロード（in‑memory）では `mode=inmem` で出力され、`loader_ratio` は概ね 0% になります。
- **build_feature_cache**: Pre-extract HalfKP features to binary cache format
  - Eliminates SFEN parsing and feature extraction overhead
  - Variable-length record format with metadata preservation
- **JSONL Support**: Direct training from annotated game data
- **Feature extraction**: HalfKP feature generation from positions

#### Training Data Generation (Streaming SFEN)

The generator now streams SFEN input to keep peak memory nearly constant, even for very large corpora. The manifest format and orchestrator integration remain unchanged.

- Input: plain text lines containing `sfen ...` (optionally with trailing `moves`), supports `-` (stdin) and compressed files (`.gz`, `.zst` when built with `zstd` feature).
- Output: JSONL or text, optional part-splitting and compression, plus v2 manifest next to outputs.
- Resume: If the output file and `<out>.progress` exist, the tool resumes automatically (skips already attempted positions and appends).

Example (streaming from stdin, JSONL output, split every 1M lines):
```bash
zcat runs/pass2_input.sfens.gz \
  | cargo run --release -p tools --bin generate_nnue_training_data -- \
      - runs/pass2.jsonl \
      --engine enhanced-nnue --output-format jsonl \
      --hash-mb 512 --multipv 2 --min-depth 3 \
      --split 1000000 --compress zst
```

Notes:
- Memory usage is bounded by the batch size, engine TT size, and output buffers — it does not grow with input size.
- When reading from `-` (stdin), input hash/size are omitted in the manifest (verification remains available for file inputs).

#### Teaching Quality Analyzer (Expected MultiPV Auto)

`analyze_teaching_quality` supports automatic MultiPV expectation resolution. The CLI accepts `--expected-multipv auto|<N>` (default: `auto`).

Resolution order when `auto`:
- Prefer final manifest field `aggregated.multipv` associated with the input
- Fallback to the (per-file) `multipv` in the nearest manifest
- If no manifest is present or fields are missing, fallback to `2`
- If a numeric value is specified at CLI, it always overrides the manifest

Example:
```bash
cargo run --release -p tools --bin analyze_teaching_quality -- \
  runs/final.jsonl --summary --manifest-autoload-mode strict
# summary line includes: "expected_mpv=<resolved>"
```

## Performance Considerations

- Use `--release` flag for production builds
- Opening book uses memory-mapped files for efficiency
- Position hashing optimized for fast lookups
- Move encoding reduces memory footprint

## License

MIT

## Threads連動の自動既定（T1/T8プロファイル）

探索開始時に `Threads` に応じた安全側の既定値を自動適用します。GUI/ユーザーが `setoption` で明示設定した値は最優先で、その項目には自動既定を上書きしません（干渉しません）。検索開始時には、実際に有効になっているプロファイルと主要パラメータを1行で出力します。

- Threads ≥ 4（T8プロファイル）
  - RootSeeGate=On, RootSeeGate.XSEE=100
  - PostVerify=On, PostVerify.YDrop=250
  - FinalizeSanity.SwitchMarginCp=30, FinalizeSanity.OppSEE_MinCp=100, FinalizeSanity.BudgetMs=8
  - MultiPV=1
- Threads = 1（T1プロファイル）
  - RootSeeGate=On, RootSeeGate.XSEE=100
  - PostVerify=On, PostVerify.YDrop=225
  - FinalizeSanity.SwitchMarginCp=35, FinalizeSanity.OppSEE_MinCp=120, FinalizeSanity.BudgetMs=4
  - MultiPV=1

出力例（検索開始時）:

```
info string effective_profile mode=Auto resolved=T8 threads=8 multipv=1 \
  root_see_gate=1 xsee=100 post_verify=1 ydrop=250 \
  finalize_enabled=1 finalize_switch=30 finalize_oppsee=100 finalize_budget=8 \
  overrides=- threads_overridden=0
```

Offモードの例（自動既定を無効化）:

```
info string effective_profile mode=Off resolved=- threads=8 multipv=1 \
  root_see_gate=0 xsee=100 post_verify=0 ydrop=300 \
  finalize_enabled=1 finalize_switch=30 finalize_oppsee=300 finalize_budget=2 \
  overrides=RootSeeGate,PostVerify threads_overridden=1
```

備考:
- すべてのオプションをGUIから明示的に`setoption`で流すタイプのGUIでは、自動既定は「そのままでは」当たりません。必要に応じて、以下のプロファイル操作を利用してください。
  - `Profile.Mode`（Auto/T1/T8/Off）: 自動既定の適用モードを選択
  - `Profile.ApplyAutoDefaults`（Button）: 主要キーの「ユーザー上書き」印をクリアし、選択中のプロファイルで自動既定を即時適用
- 超短秒（≤2秒）の局面ではcpの悪化が出やすい既知の限界があります。今後、Root Post‑Verifyのqsearch化や、finalize時のscore整合、qsearchへの「条件付き・非捕獲成り」導入で改善予定です。

秒読みの前倒しについて: `ByoyomiOverheadMs` は基礎オーバーヘッド（ネット/GUI遅延の見積り）、`ByoyomiDeadlineLeadMs` はその上に加えるリード（締切前倒し）として用いられます。純秒読み（`btime=wtime=0` かつ `byoyomi>0`）では両者の和を使って締切を計算し、`deadline_lead_applied=1` をログ出力します。
