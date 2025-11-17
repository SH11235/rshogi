# Repository Guidelines

## Project Structure & Module Organization
The repository is an npm workspace monorepo orchestrated by Turbo. Core gameplay logic lives in `packages/core/src`, typed contracts in `packages/types`, and UI layers in `packages/web/src`. Networking and platform adapters sit in `packages/server`, `packages/discord-bot`, and `packages/desktop`. WebAssembly helpers live in `packages/rust-core` (requires Rust + wasm-pack). Unit tests are colocated with implementation files using the `*.test.ts` suffix (e.g., `packages/core/src/domain/service/moveService.test.ts`).

## Build, Test, and Development Commands
Run `npm install` once to hydrate all workspaces. `npm run dev` launches package-specific dev servers via Turbo (`npm run dev --workspace=@shogi/web` for the React client, `--workspace=@shogi/server` for the Express API). `npm run build` compiles every workspace; use `npm run build --workspace=shogi-core` for focused builds. `npm test` runs the Vitest suites; prefer `npm run test:affected` before PR submission to scope execution. Lint and formatting checks are enforced with `npm run lint`, `npm run lint:fix`, and `npm run format:check`. Type safety is verified via `npm run typecheck`.

## Coding Style & Naming Conventions
Biome enforces 4-space indentation, 100-column lines, and consistent module formatting. Keep imports typed using `import type` and avoid unused symbols; the linter treats both as errors. Prefer PascalCase for exported classes, camelCase for functions and variables, and SCREAMING_SNAKE_CASE for constants. File names follow kebab-case (`move-service.ts`), and tests mirror the target file name with `.test.ts`.

## Testing Guidelines
Vitest with a `happy-dom` environment powers unit tests across packages. Write deterministic tests that interact with the public API exposed from `src/`. Use descriptive `describe` and `it` blocks that mirror the shogi concept under test. Add new suites alongside the code in `src/.../*.test.ts` so incremental runs (`npm run test:affected`) pick them up. When tests rely on board fixtures, store helpers under `src/testData/` or `__fixtures__` and keep them JSON serializable. Ensure modified rules include edge-case coverage (repetition, drop moves, promotions) before requesting review.

## Commit & Pull Request Guidelines
Follow Conventional Commit prefixes (`feat:`, `fix:`, `chore:`, `refactor:`); short Japanese summaries are acceptable when they remain under 72 characters. Group related changes per commit; re-run `npm test`, `npm run lint`, and `npm run typecheck` prior to pushing. Pull requests should explain the feature, list affected packages, and link issues or TODOs. Attach screenshots or GIFs for UI changes under `packages/web`. Flag any required Rust/WASM rebuild steps in the PR description so reviewers can reproduce the build.

## Selfplay Log Diagnostics (Rust Core)
`packages/rust-core` ships Rust CLIs for analyzing selfplay logs against the ShogiHome basic engine. Use them instead of the legacy Python scripts:

1. 生成した `runs/selfplay-basic/<timestamp>.jsonl`（+ `.info.jsonl`）に対してブランダー検出を実行します。

   ```bash
   cd packages/rust-core
   cargo run -p tools --bin selfplay_blunder_report -- \
     runs/selfplay-basic/<log>.jsonl \
     --threshold 400 \
     --back-min 0 \
     --back-max 3
   ```

   - 出力先: `runs/analysis/<log>-blunders/`（`blunders.json`, `targets.json`, `summary.txt`）。  
   - `blunders.json` は SFEN・指し手・info 行抜粋を記録するため、Coding Agent がそのまま原因を追いやすいです。

2. `targets.json` を `engine-usi` に再投入して Multi Profile で再評価します。

   ```bash
   cargo run -p tools --bin selfplay_eval_targets -- \
     runs/analysis/<log>-blunders/targets.json \
     --threads 8 --byoyomi 2000
   ```

   - `base`/`rootfull`/`gates` での評価結果を `summary.json` にまとめ、各ターゲットごとのログ（`__base.log` 等）も自動生成します。

この 2 ステップで自己対局 → ブランダー抽出 → 遡り局面再解析まで Rust ツールのみで完結します。補足的な注意事項や Python ベースのログ解析手順については `packages/rust-core/AGENTS.md` と `packages/rust-core/docs/tuning-guide.md` を参照してください。
