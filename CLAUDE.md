# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

### Development
- `npm run dev` - Start the Vite development server
- `npm run build` - Run TypeScript compilation and Vite build
- `npm run preview` - Preview the production build

### Testing
- `npm run test` - Run tests once with Vitest
- `npm run test:watch` - Run tests in watch mode
- `npm run coverage` - Generate test coverage report
- To run a single test file: `npm run test path/to/test.ts`

### Code Quality
- `npm run lint` - Run ESLint
- `npm run format` - Format code with Biome
- `npm run format:check` - Check and fix code with Biome
- `npm run typecheck` - Run TypeScript type checking without emitting files
- `npm run knip` - Check for unused dependencies and exports

### Storybook
- `npm run storybook` - Start Storybook development server
- `npm run build-storybook` - Build Storybook for production

## Architecture

This is a Shogi (Japanese Chess) web application built with React and TypeScript. The codebase follows Domain-Driven Design principles with clear separation of concerns:

### Monorepo Structure
- `packages/core/` - Shared domain logic and types (imported as "shogi-core")
- `packages/web/` - React web application
- Dependencies: web package depends on core package for game logic

### Current Implementation (packages/web/)

#### Components (`/src/components/`)
- `Board.tsx` - 9x9 shogi board with square click handling
- `Piece.tsx` - Individual piece rendering with Japanese characters
- `GameInfo.tsx` - Game status, turn indicator, and controls
- `CapturedPieces.tsx` - Display captured pieces (hands) for both players
- `ui/` - shadcn/ui component library (Button, Card, Input, Table)

#### State Management (`/src/stores/`)
- `gameStore.ts` - Zustand store with game state, move validation, and history
- Centralized state for board, current player, selected square, valid moves, hands

#### Key Domain Concepts
- **Board**: Record<SquareKey, Piece | null> (e.g., "11" → piece or null)
- **Square**: Position {row: 1-9, column: 1-9}
- **Piece**: Japanese pieces with owner and promoted status
- **Move**: NormalMove (board moves) or DropMove (placing captured pieces)
- **Hands**: Captured pieces available for dropping

#### UI Library & Styling
- **shadcn/ui**: Modern component library with Radix UI primitives
- **Tailwind CSS**: Utility-first CSS framework
- **Storybook**: Component development and documentation tool
- Components: Button, Card, Input, Table (configured and tested)

#### Testing Strategy
- **Framework**: Vitest with @testing-library/react
- **Coverage**: 83 tests across 8 test files
- **Test Types**: Unit tests for utils, component tests, store integration tests
- **Co-location**: Tests next to source files (`.test.ts/.test.tsx`)
- **Setup**: Global test utilities, happy-dom environment

#### Development Notes
- TypeScript strict mode enabled, NO "any" type usage allowed
- Path alias `@/*` maps to `src/*`
- Biome for linting and formatting (4 spaces, 100 char line width)
- ESLint rules: prefer for-of over forEach, explicit button types

## Code Quality Standards

**IMPORTANT**: After completing each coding task, ALWAYS run the following checks in order:

1. **Build check**: `npm run build` - Ensures code compiles without errors
2. **Type check**: `npm run typecheck` - Verifies TypeScript types are correct
3. **Format check**: `npm run format:check` - Applies automatic code formatting
4. **Lint check**: `npm run lint` - Ensures code follows linting rules

These checks must all pass before considering a task complete. This ensures code quality and prevents build failures in CI/CD pipelines.

### Type Safety Requirements
- **NO "any" type usage** - Use proper TypeScript types always
- **Use type guards** for union types (Move, etc.)
- **Explicit button types** - Always specify `type="button"`
- **Prefer for-of over forEach** - Especially in test files

## Rust/WebAssembly Development (packages/rust-core/)

### Commands

**IMPORTANT**: When modifying any Rust code in packages/rust-core/, ALWAYS run these commands in order:

1. **Build WASM**: `npm run build:wasm` (from root) or `make build` (from rust-core) - Builds the WebAssembly module
2. **Test**: `cargo test` - Runs standard Rust tests
3. **Format**: `cargo fmt` - Formats Rust code according to standard style
4. **Lint**: `cargo clippy` - Runs Rust linter for code quality

### Development Workflow

When working with rust-core:
1. Make changes to Rust code
2. Run `npm run build:wasm` from packages/rust-core directory
3. The built WASM files are automatically copied to packages/web/src/wasm/
4. Test changes in the web application

### Testing Strategy

- **Browser Tests**: Use `#[wasm_bindgen_test]` for WASM-specific tests
- **Unit Tests**: Use standard `#[test]` for pure Rust logic
- **Test Runner**: `wasm-pack test --chrome --headless`
- Keep tests minimal focusing on core functionality

### Code Quality Standards

- Use proper error handling with Result types where applicable
- Minimize JavaScript interop surface area
- Document public APIs with doc comments
- Keep WASM module size small by avoiding unnecessary dependencies

### Build Configuration

- Target: wasm32-unknown-unknown
- Build tool: wasm-pack
- Output: ES modules with TypeScript definitions
- Optimization: Enable in release builds

## Additional Documentation

For detailed references to reduce token usage, see:
- **packages/web/.claude/file-structure.md** - File locations and import patterns
- **packages/web/.claude/type-definitions.md** - Key TypeScript types reference
- **packages/web/.claude/common-patterns.md** - Code patterns and best practices
- **packages/web/.claude/quick-reference.md** - Essential commands and workflows
