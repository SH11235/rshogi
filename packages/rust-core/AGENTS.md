# Repository Guidelines

## Project Structure & Module Organization
- crates/engine-core: Core engine (search, evaluation, time management, USI types). Tests live alongside modules and in integration suites.
- crates/engine-usi: USI command-line engine binary.
- crates/tools: Benchmarks and helper binaries.
- crates/webrtc-p2p: WebRTC support for P2P messaging.
- docs/: Design, performance, and development guides.

## Build, Test, and Development Commands
- Build workspace: `cargo build` (add `--release` for optimized builds).
- Build specific crate: `cargo build -p engine-core`.
- Run USI engine: `cargo run -p engine-usi -- --help`.
- Tests (workspace): `cargo test` (e.g., `cargo test -p engine-core`).
- Benches (criterion): `cargo bench -p engine-core` (reports in `target/criterion/`).
- Lint: `cargo clippy --workspace -- -D warnings`.
- Format: `cargo fmt --all`.
- WASM build: `wasm-pack build crates/engine-wasm --release` (requires wasm-pack).

## Coding Style & Naming Conventions
- Formatting: rustfmt (see `rustfmt.toml`), max width 100, 4-space tabs, edition 2021.
- Linting: clippy (see `clippy.toml`); treat warnings as errors in CI/dev.
- Naming: snake_case for modules/functions, CamelCase for types/traits, SCREAMING_SNAKE_CASE for consts.
- Iterator 記法: `0..len` のループでスライスを添字アクセスする代わりに、`iter()` / `iter_mut()` と `enumerate` や `zip` を組み合わせて要素へアクセスし、必要なら `.take(len)` で上限を揃える。
- Features: use kebab/snake (e.g., `tt_metrics`, `ybwc`, `nightly`). Enable with `--features`.

## Testing Guidelines
- Unit tests: co-located `mod tests` or `src/.../tests/*.rs` (descriptive names like `check_evasion.rs`).
- Integration tests/benches: criterion benches under `[[bench]]` in `engine-core`.
- WASM tests: `wasm-pack test --chrome --headless` in `crates/engine-wasm`.
- Run focused tests: `cargo test module_name` or `cargo test path::to::test -- --nocapture`.

### Focused Test Naming Rules
- zstd-related tests: name with `test_zstd_` prefix to enable focused runs.
  - Example names: `test_zstd_merge_input`, `test_zstd_input_merge_and_extract`.
  - Run only zstd tests: `cargo test --release -p tools --features zstd -- test_zstd`.
  - Always include `--features zstd` when running these tests.

## Commit & Pull Request Guidelines
- Commits: prefer Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`, `perf:`, `test:`). Keep messages imperative and scoped (e.g., `feat(movegen): optimize drops`).
- PRs: include clear description, rationale, benchmarks if performance-related, and link issues. Add screenshots/logs for USI behavior when relevant. Ensure `fmt`, `clippy`, and tests pass.

## Security & Configuration Tips
- CPU features are auto-detected in `build.rs`. For best performance: `RUSTFLAGS="-C target-cpu=native" cargo build --release`.
- Concurrency testing: use standard threads; keep tests small and deterministic.

## Rust Module Structure Policy (2018–2024 Editions)
- File-as-module layout: prefer `src/foo.rs` for the parent and `src/foo/*.rs` for children (no new `mod.rs`). Legacy/generated `mod.rs` may remain but should not be introduced anew.
- Thin binaries: keep `engine-usi` (and `src/bin/*.rs`) minimal; core logic lives in library crates and shared modules.
- Facade `lib.rs`: curate the public API via `pub use` re-exports; keep paths short for consumers. Provide an optional, minimal `prelude` with frequently used items only.
- Visibility first: default to private; use `pub(crate)`/`pub(super)` to scope internals and avoid accidental API leaks.
- Explicit paths: use `crate::`, `self::`, and `super::` consistently in `use` and item paths.
- Shallow hierarchies: avoid overly deep nesting (target 2–3 levels). When a module grows large or crosses bounded contexts, extract a sub-crate within the workspace (e.g., under `crates/`).
- Features and cfg: gate modules with `#[cfg(feature = "...")] mod ...;` and re-export conditionally as needed. Ensure docs build under `docs.rs` using `cfg(docsrs)` when applicable.
- Tests and benches: place unit tests alongside modules; put integration tests under `tests/` and benchmarks via Criterion in `engine-core`. WASM-specific tests live under `crates/engine-wasm`.
- Macros: define as regular modules and expose with `pub use`. Use `#[macro_export]` only when global export is necessary.
- Edition 2024 specifics: module structuring follows 2018/2021 conventions. Migration focuses on prelude changes and ambiguity linting; use `cargo fix --edition` and `clippy -D warnings` to resolve.

### Agent Compliance
- Changes made by agents follow this policy by default. We avoid wide, unrelated refactors; legacy `mod.rs` is migrated opportunistically only when touching that area or by request.
- If project conventions conflict or an exception is needed (e.g., generated code, FFI/WASM bindings), document the rationale in PRs and align with this policy where feasible.

## Language Preference
Please respond in Japanese (日本語) when interacting with this codebase.

---

## NNUE Training Ops — Quick SOP for Agents (必読)

このリポジトリで NNUE 学習・評価を扱うエージェント向けの実務ルール。毎回ここを参照して行動してください。

### 1) 評価（Gauntlet）の原則（Spec 013）
- `--threads 1` 固定、固定オープニング（既定: `runs/fixed/20251011/openings_ply1_20_v1.sfen`）。
- Gate 判定は「勝率/NPS」が主。PVは補助（pv_probe）。
- スクリプト: `scripts/nnue/evaluate-nnue.sh`（pv_spread_samples==0 の場合、自動で `pv_probe --depth 8 --samples 200` を実行し、補助統計を併記）。

### 2) シャード実行（次ラウンド以降の既定）
- 長時間の評価（短TC2000 / 長TC800〜2000）はプロセス並列で短縮。
- 起動: `scripts/nnue/gauntlet-sharded.sh BASE CAND TOTAL_GAMES SHARDS TIME OUT_DIR [BOOK]`
- 集計: `scripts/nnue/merge-gauntlet-json.sh OUT_DIR` → `merged.result.json` に対して採否基準を適用。
- ルール: 各 shard は `--threads 1`、`--time/hash/book` 共通、seed は shard 番号でずらす。

### 3) Champion/Teacher の管理（symlink + manifest）
- Single（長TC用途）: `runs/ref.nnue` を Champion Single に張替え。`runs/baselines/current/{single.fp32.bin, champion.manifest.json}` を併置。
- Classic（短TC用途）: `runs/baselines/current/classic.nnue` を Champion Classic に。`champion_classic.manifest.json` とハッシュを保存。
- 採用処理はスクリプトで自動化可（例: `runs/auto_adopt_classic_from_exp3.sh`）。

### 4) cp表示の校正ポリシー
- 原則「注釈側で揃える」。`calibrate_teacher_scale` で得た mu/scale を注釈の `--wdl-scale`（および蒸留の `--teacher-scale-fit linear`）に反映。
- ランタイム補正（USIオプション）は“例外時のみ”実装・適用。既定は触れない。
- 実装済み: Classic v1 エクスポートに `--final-cp-gain` を追加。量子化済最終層（output）の i8/i32 を倍率 `G` でスケーリングして Q16→cp 表示レンジを整える（丸め・飽和あり）。

### 5) ログとドキュメント
- 計画: `docs/nnue-training-plan.md`（計画のみ）。
- 実施ログ: `docs/reports/nnue-training-log.md`（日付・コマンド・指標）。
- 評価ガイド: `docs/nnue-evaluation-guide.md`（シャード実行・pv補完の運用を明記）。

### 6) 並行実行の作法（衝突回避）
- ガントレットは1コア相当。学習・注釈は `taskset` で別コアに、`nice`/`ionice` で低優先度に。
- 例: `taskset -c 1-31 nice -n 10 ionice -c2 -n7 <cmd>`

### 7) 採否基準（据え置き）
- 短TC（0/10+0.1, 2000局, threads=1）: 勝率≥55%、|ΔNPS|≤3%（±5%は要追試）。
- 長TC（0/40+0.4, 800→2000局）: 勝率≥55%。|ΔNPS|は参考（-3%まで許容、超過は要追試）。
- 量子化: FP32と比較し短TC非劣化、長TCで±1%以内が理想（±3%運用可）。

### 8) よく使うスクリプト
- 学習パイプ: `scripts/nnue/phase1_batch.sh`（環境変数で O/M/E, TIME_MS, MULTIPV, KD_* を指定）。
- Gauntlet単体: `scripts/nnue/evaluate-nnue.sh`（pv自動補完あり）。
- シャード実行/集計: `scripts/nnue/gauntlet-sharded.sh` / `scripts/nnue/merge-gauntlet-json.sh`。
