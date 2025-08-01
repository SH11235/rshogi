# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Principles
- Test-driven development is mandatory based on code excellence principles
- Code must be type-safe, with no usage of "any" type
- When practicing TDD and Test Driven Development, please follow all the steps recommended by t-wada.
- Applied for the Kent C. Dodds Testing Trophy for front-end testing
- For refactoring, follow the steps recommended by Martin Fowler.

## Commands

### Development
- `npm run dev` - Start the Vite development server
- `npm run build` - Run TypeScript compilation and Vite build
- `npm run preview` - Preview the production build

### Testing
- `npm run test` - Run tests once with Vitest
- `npm run test:unit` - Run unit tests only
- `npm run test:watch` - Run tests in watch mode
- `npm run coverage` - Generate test coverage report
- `npm run test:e2e` - Run Playwright end-to-end tests
- `npm run test:e2e:ui` - Run Playwright tests with UI mode
- To run a single test file: `npm run test path/to/test.ts`

### Code Quality
- `npm run lint` - Run Biome linter
- `npm run lint:fix` - Run Biome linter with auto-fix
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
- `Board.tsx` - 9x9 shogi board with square click handling and move visualization
- `Piece.tsx` - Individual piece rendering with Japanese characters
- `GameInfo.tsx` - Game status, turn indicator, and game controls
- `CapturedPieces.tsx` - Display captured pieces (hands) for both players
- `MoveHistory.tsx` - Move history display with navigation
- `Timer.tsx` & `TimerDisplay.tsx` - Game timer functionality
- `PromotionDialog.tsx` - Piece promotion selection dialog
- `KifuImportDialog.tsx` & `KifuExportDialog.tsx` - KIF format import/export
- `OnlineGameDialog.tsx` - WebRTC-based online multiplayer setup
- `AIGameSetup.tsx` & `AIStatus.tsx` - AI opponent configuration and status
- `SavedGamesDialog.tsx` - Local game save/load functionality
- `GameRecordManager.tsx` - Game recording and management
- `MateSearchPanel.tsx` - Mate search analysis tool
- `KeyboardHelp.tsx` - Keyboard shortcuts reference
- `DrawOfferDialog.tsx` - Draw offer handling for online games
- `OpeningBook/` - Opening book display and selection
- `ui/` - shadcn/ui component library (Button, Card, Dialog, Input, Table, Tabs, etc.)

#### State Management (`/src/stores/`)
- `gameStore.ts` - Zustand store with game state, move validation, and history
- Centralized state for board, current player, selected square, valid moves, hands
- Includes move history management, game recording, and state persistence

#### Custom Hooks (`/src/hooks/`)
- `useKeyboardShortcuts.ts` - Keyboard navigation and shortcuts
- `useGameSettings.ts` - Game settings management with persistence
- `useLocalStorage.ts` - Type-safe localStorage wrapper
- `useAudio.ts` - Audio playback hook for sound effects
- `useOpeningBook.ts` - Opening book data access

#### Type Definitions (`/src/types/`)
- `ai.ts` - AI player, difficulty levels, and worker message types
- `audio.ts` - Audio system types and configurations
- `online.ts` - WebRTC and online game message types
- `timer.ts` - Game timer settings and state types
- `mateSearch.ts` - Mate search analysis types
- `openingBook.ts` - Opening book data structures
- `settings.ts` - Game settings and preferences

#### Key Domain Concepts
- **Board**: Record<SquareKey, Piece | null> (e.g., "11" → piece or null)
- **Square**: Position {row: 1-9, column: 1-9}
- **Piece**: Japanese pieces with owner and promoted status
- **Move**: NormalMove (board moves) or DropMove (placing captured pieces)
- **Hands**: Captured pieces available for dropping

#### Services (`/src/services/`)
- `ai/aiService.ts` - AI opponent implementation with WebWorker integration
- `webrtc.ts` - WebRTC connection management for online play
- `audioManager.ts` & `audioGenerator.ts` - Sound effects and audio management
- `openingBook.ts` & `wasmOpeningBookLoader.ts` - Opening book loading and management
- `online/` - Online game services and signaling

#### UI Library & Styling
- **shadcn/ui**: Modern component library with Radix UI primitives
- **Tailwind CSS**: Utility-first CSS framework (v4 with Vite integration)
- **Storybook**: Component development and documentation tool
- Components: Button, Card, Dialog, Input, Table, Tabs, Alert, Select, Radio, ScrollArea, etc.

#### Testing Strategy
- **Framework**: Vitest with @testing-library/react
- **Coverage**: 28 test files distributed across components, stores, hooks, and services
- **Test Types**: Unit tests for utils, component tests, store integration tests, hook tests
- **Co-location**: Tests next to source files (`.test.ts/.test.tsx`)
- **Setup**: Global test utilities in `test/setup.ts`
- **E2E Testing**: Playwright for end-to-end testing with MCP support

#### Development Notes
- TypeScript strict mode enabled, NO "any" type usage allowed
- Path alias `@/*` maps to `src/*`
- Biome for linting and formatting (4 spaces, 100 char line width)
- Linting rules: prefer for-of over forEach, explicit button types
- WASM integration for opening book functionality
- WebWorker for AI processing to maintain UI responsiveness

## Code Quality Standards

**IMPORTANT**: After completing each coding task, ALWAYS run the following checks:

1. **Build check**: `npm run build` - Ensures code compiles without errors
2. **Type check**: `npm run typecheck` - Verifies TypeScript types are correct

Note: Format and lint checks are automatically run via Hooks after file edits.

### Type Safety Requirements
- **NO "any" type usage** - Use proper TypeScript types always
- **Use type guards** for union types (Move, etc.)
- **Explicit button types** - Always specify `type="button"`
- **Prefer for-of over forEach** - Especially in test files

## Rust/WebAssembly Development (packages/rust-core/)

### Engine Type Selection

**推奨設定**: `EnhancedNnue` - 最強の組み合わせ（高度な探索技術 + NNUE評価関数）

```bash
# USIプロトコルで設定
setoption name EngineType value EnhancedNnue
```

詳細は `packages/rust-core/docs/engine-types-guide.md` を参照。

### Commands

**IMPORTANT**: When modifying any Rust code in packages/rust-core/, ALWAYS run these commands in order:

1. **Build WASM**: `npm run build:wasm` (from root) or `make build` (from rust-core) - Builds the WebAssembly module
2. **Test**: `cargo test` - Runs standard Rust tests
3. **Format**: `cargo fmt` - Formats Rust code according to standard style
4. **Lint**: `cargo clippy` - Runs Rust linter for code quality

### Development Workflow

When working with rust-core:
1. Make changes to Rust code
2. Run quality checks: `cargo fmt`, `cargo clippy`, `cargo test`
3. Run `npm run build:wasm` from packages/rust-core directory
4. The built WASM files are automatically copied to packages/web/src/wasm/
5. Test changes in the web application

### Code Quality Tools

**Required checks (run automatically on pre-commit):**
- `cargo fmt` - Code formatting according to Rust style guidelines
- `cargo clippy -- -D warnings` - Linting with warnings as errors
- `cargo check` - Fast type checking without code generation

**Additional quality tools:**
- `cargo test` - Run all tests
- `cargo audit` - Check for security vulnerabilities in dependencies
- `cargo outdated` - Check for outdated dependencies
- `cargo machete` - Find unused dependencies (requires installation)

**Quick commands:**
- `make check-all` - Run all quality checks at once
- `make format-check` - Check formatting without modifying files
- `make install-dev-tools` - Install additional development tools

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

### Debug Tools

**Position Analysis Tool**: Use `debug_position` for investigating search performance issues

```bash
# Analyze specific position with SFEN
cargo run --release --bin debug_position -- --sfen "SFEN" --depth 5 --time 1000

# Compare engine types
cargo run --release --bin debug_position -- -s "SFEN" -e material
cargo run --release --bin debug_position -- -s "SFEN" -e enhanced_nnue

# Check move generation
cargo run --release --bin debug_position -- -s "SFEN" --moves
cargo run --release --bin debug_position -- -s "SFEN" --perft 5
```

See `/packages/rust-core/docs/debug-position-tool.md` for detailed usage.

### Build Configuration

- Target: wasm32-unknown-unknown
- Build tool: wasm-pack
- Output: ES modules with TypeScript definitions
- Optimization: Enable in release builds

## Web Search

**IMPORTANT**: When performing web searches, use the gemini CLI command via Task tool instead of the built-in WebSearch tool:

```bash
gemini -p "WebSearch: [your search query]"
```

This approach is preferred as it provides project-specific context and better integration with the development workflow.

## Discord Conversation Logger Rules

### Important Message Logging Rules

Please use discord-conversation-logger MCP to log messages in the following cases:

#### 1. User Messages (human)
- Task start/change/completion instructions
- Important decisions or confirmations
- Error reports or issue identification

#### 2. Assistant Messages (assistant)
- Task completion reports
- Important suggestions or solutions
- Error resolution methods
- Summary of significant changes made

#### 3. System Messages (system)
- Critical errors or warnings
- Important environment changes
- Security-related notifications

### Logging Format

```
mcp__discord-conversation-logger__log_conversation(
  message: "Actual message content",
  role: "human" | "assistant" | "system",
  context: "Brief context description"
)
```

### Cases Not Requiring Logs
- Simple acknowledgments
- Intermediate progress reports
- Temporary debug outputs


## WebRTC Communication Patterns

### Error Handling and Reconnection
- Implement exponential backoff for reconnection attempts
- Store connection state in Zustand with proper error states
- Use try-catch blocks for all WebRTC operations
- Provide user feedback for connection states

Example reconnection pattern:
```typescript
private async attemptReconnect(attempt: number = 0): Promise<void> {
    if (attempt >= this.MAX_RECONNECT_ATTEMPTS) {
        this.handleError(new WebRTCError("Max reconnection attempts reached", "MAX_ATTEMPTS"));
        return;
    }
    
    const delay = Math.min(1000 * Math.pow(2, attempt), 30000); // Exponential backoff
    await new Promise((resolve) => setTimeout(resolve, delay));
    
    try {
        // Reconnection logic
    } catch (error) {
        await this.attemptReconnect(attempt + 1);
    }
}
```

### Message Type Extension
- Define all message types in `types/online.ts`
- Create type guards for each message type
- Use discriminated unions for type safety
- Add new message types to the GameMessage union

Example pattern:
```typescript
export interface NewFeatureMessage extends GameMessage {
    type: "new_feature";
    data: { /* specific data */ };
}

export function isNewFeatureMessage(msg: GameMessage): msg is NewFeatureMessage {
    return msg.type === "new_feature";
}
```

## State Management Best Practices

### Complex State with Zustand
- Separate concerns into logical slices (game, connection, timer, etc.)
- Use immer for immutable updates when needed
- Implement proper error boundaries
- Clear state appropriately on disconnection

### Async Operations
- Handle all async operations in try-catch blocks
- Update loading states appropriately
- Provide user feedback for long operations
- Use proper cleanup in useEffect hooks

### State Synchronization
- Send state updates immediately after local changes
- Validate received state before applying
- Handle race conditions with timestamps
- Implement optimistic updates where appropriate

## Type Safety Patterns

### Avoiding "any" Type
- Use `unknown` for untyped data from external sources
- Create proper type guards for runtime validation
- Use generics for reusable components
- Leverage TypeScript's discriminated unions

Example of unknown usage:
```typescript
// Instead of: board: any
board: Record<string, unknown>; // Then validate with type guards

// For complex types
function isValidBoard(board: unknown): board is Board {
    // Validation logic
}
```

### Message Handling
- Define strict types for all messages
- Use exhaustive checks in switch statements
- Implement proper error types
- Validate all external data

## Testing Strategies

### WebRTC Testing
- Mock WebRTC connections for unit tests
- Use vi.mock() for module mocking
- Test error scenarios thoroughly
- Implement connection state assertions

### Async Testing
- Use proper async/await in tests
- Mock timers for reconnection logic
- Test race conditions
- Verify cleanup on unmount

## AI Engine Architecture

### AI Service Implementation
The AI opponent is implemented through a service layer with WebWorker integration:

#### AI Service (`services/ai/aiService.ts`)
- Manages AI player state and difficulty levels (beginner, intermediate, advanced, expert)
- Communicates with WebWorker for move calculation
- Handles position evaluation requests
- Provides async interface for UI interaction

#### WebWorker Integration (`workers/aiWorker.ts`)
- Runs AI calculations in background thread to maintain UI responsiveness
- Handles move calculation and position evaluation messages
- Implements difficulty-based time limits and search depth
- Prevents UI blocking during complex calculations

#### AI Features
- **Difficulty Levels**: Four skill levels from beginner to expert
- **Time Management**: Configurable thinking time per difficulty
- **Position Evaluation**: Real-time position assessment
- **Error Handling**: Graceful error recovery with fallback moves
- **State Management**: Tracks AI thinking state for UI feedback

## Additional Documentation

For detailed references to reduce token usage, see:

### Implementation Guides
- **docs/webrtc-patterns.md** - WebRTC implementation patterns
- **docs/testing-strategies.md** - Comprehensive testing guide
- **docs/state-management-patterns.md** - Zustand patterns and examples

### Development References
- **packages/web/.claude/type-definitions.md** - Key TypeScript types reference
- **packages/web/.claude/common-patterns.md** - Code patterns and best practices
- **packages/web/.claude/quick-reference.md** - Essential commands and workflows
- **packages/web/.claude/webrtc-message-types.md** - Message type extension guide
- **packages/web/.claude/refactoring-patterns.md** - Core package migration patterns
