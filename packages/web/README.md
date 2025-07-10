# @shogi/web

React-based web interface for the Shogi game engine.

## Features

- 🎮 Full Shogi game implementation with all standard rules
- 🤖 AI opponent with 4 difficulty levels (Beginner to Expert)
- 🔍 Mate search analysis up to 7 moves
- ⏱️ Multiple time control systems (Sudden Death, Byoyomi, Fischer)
- 🌐 WebRTC peer-to-peer multiplayer
- 📚 Opening book integration with 100,000+ positions
- 🎵 Audio feedback for moves and game events
- 💾 Game save/load functionality
- 📜 KIF format import/export support
- ⌨️ Keyboard shortcuts for efficient play
- 📱 Responsive design for mobile and desktop

## Architecture

### Directory Structure
```
src/
├── components/          # React components
│   ├── OpeningBook/    # Opening book UI components
│   └── ui/             # shadcn/ui components
├── contexts/           # React contexts
├── hooks/              # Custom React hooks
├── services/           # Business logic
│   ├── ai/            # AI engine implementation
│   └── online/        # WebRTC communication
├── stores/            # Zustand state management
├── types/             # TypeScript type definitions
├── utils/             # Utility functions
├── wasm/              # WebAssembly modules (generated)
└── workers/           # Web Workers

```

### State Management
- **Zustand** for global game state
- **React hooks** for component-level state
- **WebWorkers** for AI processing

### Key Components
- `Board.tsx` - Main 9x9 game board with interactive squares
- `GameInfo.tsx` - Game status, controls, and information panel
- `CapturedPieces.tsx` - Display of captured pieces (hands)
- `Timer.tsx` - Game timer with multiple time control modes
- `MoveHistory.tsx` - Move history with algebraic notation
- `OnlineGameDialog.tsx` - WebRTC multiplayer setup
- `OpeningBook.tsx` - Opening book move suggestions
- `MateSearchPanel.tsx` - Mate search analysis interface

### AI Engine
Located in `src/services/ai/`:
- `aiService.ts` - Main AI interface and worker management
- `src/workers/aiWorker.ts` - AI worker implementation using shogi-core

## Development

### Prerequisites
- Node.js v24.x (managed by Volta)
- npm v11.x
- Rust toolchain (for building WASM modules)

### Setup
```bash
# Install dependencies
npm install

# Build WASM modules (required before first run)
npm run build:wasm

# Start development server
npm run dev
```

### Commands
```bash
# Development
npm run dev           # Start Vite development server
npm run build         # TypeScript compilation and Vite build
npm run preview       # Preview production build

# Testing
npm test              # Run tests once with Vitest
npm test:watch        # Run tests in watch mode
npm run coverage      # Generate test coverage report

# Code Quality
npm run lint          # Run ESLint
npm run format        # Format code with Biome
npm run format:check  # Check and fix code with Biome
npm run typecheck     # Run TypeScript type checking
npm run knip          # Check for unused dependencies

# Storybook
npm run storybook     # Start Storybook development server
npm run build-storybook # Build Storybook for production
```

### Testing Strategy
- **Framework**: Vitest with @testing-library/react
- **Test Files**: 28 test files covering components, hooks, stores, and utilities
- **Test Types**: Unit tests, component tests, integration tests
- **Co-location**: Tests are placed next to source files (`.test.ts/.test.tsx`)
- **Coverage**: Comprehensive coverage across all major features

## AI Implementation

### AI Engine Architecture
The AI runs in a Web Worker using the shogi-core npm package:
- **Worker**: `src/workers/aiWorker.ts` - Handles AI computations
- **Service**: `src/services/ai/aiService.ts` - Manages worker communication
- **Core Logic**: Provided by the `shogi-core` npm package

### Difficulty Levels
- **Beginner**: Quick responses, basic evaluation
- **Intermediate**: Moderate depth search
- **Advanced**: Deeper search with better evaluation
- **Expert**: Maximum search depth with full features

### Features
- Move generation and validation
- Position evaluation
- Search algorithms with pruning
- Opening book integration
- Mate search capabilities

## Code Quality Standards

- **TypeScript**: Strict mode enabled, NO `any` types allowed
- **Testing**: Test-driven development approach
- **Formatting**: Automated with Biome (4 spaces, 100 char line width)
- **Linting**: ESLint with custom rules
- **Git Hooks**: Pre-commit checks for formatting and linting
- **Type Safety**: Comprehensive type definitions in `src/types/`

## WebRTC Multiplayer

### Features
- Peer-to-peer connection using WebRTC
- Room-based matchmaking
- Real-time move synchronization
- Connection status monitoring
- Automatic reconnection on disconnect

### Implementation
- **Service**: `src/services/webrtc.ts` - WebRTC connection management
- **Types**: `src/types/online.ts` - Message type definitions
- **Components**: `OnlineGameDialog.tsx`, `ConnectionStatus.tsx`

## Future Plans

- [x] Opening book database (100,000+ positions)
- [ ] Endgame tablebase
- [ ] Analysis mode with multiple lines
- [x] Game recording and replay (KIF format)
- [ ] Tournament support
- [ ] Cloud game storage
- [ ] Mobile app versions

## License

MIT