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
