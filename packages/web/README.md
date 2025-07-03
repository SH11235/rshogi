# @shogi/web

React-based web interface for the Shogi game engine.

## Features

- 🎮 Full Shogi game implementation with all rules
- 🤖 AI opponent with 4 difficulty levels
- 🔍 Mate search up to 7 moves
- ⏱️ Multiple time control systems
- 🌐 WebRTC peer-to-peer multiplayer (experimental)
- 📱 Responsive design for mobile and desktop

## Architecture

### State Management
- **Zustand** for global game state
- **React hooks** for component-level state
- **WebWorkers** for AI processing

### Key Components
- `Board.tsx` - Main game board with piece rendering
- `GameInfo.tsx` - Game status, controls, and information
- `CapturedPieces.tsx` - Display of captured pieces (hands)
- `OnlineGame.tsx` - WebRTC multiplayer interface

### AI Engine
Located in `src/services/ai/`:
- `engine.ts` - Main AI interface and game logic
- `evaluation.ts` - Advanced position evaluation
- `search.ts` - Iterative deepening with alpha-beta pruning

## Development

### Prerequisites
- Node.js v24.x (managed by Volta)
- npm v11.x

### Setup
```bash
npm install
npm run dev
```

### Testing
```bash
npm test           # Run all tests
npm test:watch     # Run tests in watch mode
npm run coverage   # Generate coverage report
```

### Building
```bash
npm run build      # Production build
npm run preview    # Preview production build
```

## AI Implementation

### Difficulty Levels
- **Beginner**: 2-ply search, 1 second thinking time
- **Intermediate**: 4-ply search, 3 seconds
- **Advanced**: 6-ply search, 5 seconds
- **Expert**: 8-ply search, 10 seconds

### Search Features
- Iterative deepening for time management
- Alpha-beta pruning with move ordering
- Transposition table for position caching
- Killer move heuristic
- MVV-LVA capture ordering

### Evaluation Components
- Material balance with traditional piece values
- Piece-square tables for positional play
- King safety evaluation
- Piece mobility assessment
- Piece coordination bonuses

## Code Quality

- TypeScript strict mode enabled
- No `any` types allowed
- Comprehensive test coverage (189 tests)
- Automated formatting with Biome
- Git hooks for pre-commit checks

## Future Plans

- [ ] Opening book database
- [ ] Endgame tablebase
- [ ] Analysis mode with multiple lines
- [ ] Game recording and replay
- [ ] Tournament support

## License

MIT