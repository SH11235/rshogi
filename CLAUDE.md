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

## Architecture

This is a Shogi (Japanese Chess) web application built with React and TypeScript. The codebase follows Domain-Driven Design principles with clear separation of concerns:

### Domain Layer (`/src/domain/`)
The core game logic is isolated from UI concerns:
- `model/` - Domain entities (Board, Piece, Move, Square)
- `service/` - Game rules and logic (move validation, checkmate detection)
- `initialBoard.ts` - Initial game state
- `utils/` - Utility functions for notation

### State Management (`/src/state/`)
Uses Zustand with persist middleware for game state management:
- `gameStore.ts` - Central store managing board state, move history, turn tracking, captured pieces (hands), and game results
- Supports undo/redo functionality through cursor-based history navigation

### Key Domain Concepts
- **Board**: Record of square keys to pieces (e.g., "11" → piece or null)
- **Square**: Position on the 9x9 board (row: 1-9, column: 1-9)
- **Piece**: Japanese pieces (歩=Pawn, 香=Lance, 桂=Knight, 銀=Silver, 金=Gold, 角=Bishop, 飛=Rook, 王/玉=King)
- **Move**: Either a board move (from/to squares) or a drop move (placing captured piece)
- **Hands**: Captured pieces available for dropping back onto the board

### Testing Strategy
- Tests are co-located with source files (`.test.ts`)
- Comprehensive unit tests for domain logic
- Uses Vitest with happy-dom for DOM testing
- Global test utilities (expect, vi) available without imports

### Development Notes
- TypeScript strict mode is enabled
- Path alias `@/*` maps to `src/*`
- Biome is used for formatting (4 spaces, 100 char line width)
- Pre-commit hooks run formatting via Husky and lint-staged
- Node.js version is managed by Volta (v24.0.1)