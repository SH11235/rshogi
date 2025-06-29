# Refactoring Patterns: Core Package Migration

This document outlines patterns and best practices for migrating logic from the web package to the core package.

## When to Move to Core

### Criteria for Core Migration
1. **Domain Logic**: Pure game rules, move validation, board operations
2. **Platform Agnostic**: No UI dependencies, no browser APIs
3. **Reusable**: Could be used by CLI, mobile app, or server
4. **Testable**: Pure functions with no side effects
5. **Type Definitions**: Shared types and interfaces

### What Stays in Web
1. **UI Components**: React components, hooks
2. **Browser APIs**: localStorage, WebRTC, audio
3. **State Management**: Zustand stores
4. **Event Handlers**: Click handlers, keyboard shortcuts
5. **Styling**: CSS, Tailwind classes

## Migration Process

### Step 1: Identify Dependencies
```typescript
// Before migration, map out dependencies
// Example: validateMove.ts

// External dependencies:
import type { Board, Move, Player } from "shogi-core"; // ✅ Already in core

// Internal dependencies:
import { isInCheck } from "./checkmate"; // ❌ Need to migrate
import { generateLegalMoves } from "./moveGeneration"; // ❌ Need to migrate
```

### Step 2: Create Core Module Structure
```bash
packages/core/src/
├── domain/
│   ├── model/      # Types and interfaces
│   ├── service/    # Business logic
│   └── value/      # Value objects
├── index.ts        # Public API exports
└── tests/          # Unit tests
```

### Step 3: Migrate Types First
```typescript
// 1. Move type definitions to core/src/domain/model/
// packages/core/src/domain/model/timer.ts
export interface TimerConfig {
    mode: "basic" | "fischer" | "perMove" | null;
    basicTime: number;
    byoyomiTime: number;
    // ...
}

// 2. Update imports in web package
// Before: import { TimerConfig } from "@/types/timer";
// After: import type { TimerConfig } from "shogi-core";
```

### Step 4: Migrate Pure Functions
```typescript
// Before: packages/web/src/utils/boardUtils.ts
export function parseSquareKey(key: string): Square {
    const row = parseInt(key[0]);
    const column = parseInt(key[1]);
    return { row, column };
}

// After: packages/core/src/domain/service/boardService.ts
export function parseSquareKey(key: SquareKey): Square {
    const row = Number.parseInt(key[0]);
    const column = Number.parseInt(key[1]);
    return { row, column };
}
```

### Step 5: Update Exports
```typescript
// packages/core/src/index.ts
// Domain models
export type { Board, Piece, Square, Move } from "./domain/model";

// Services
export { 
    validateMove,
    generateLegalMoves,
    isInCheck,
    isCheckmate 
} from "./domain/service";

// Make sure to maintain backward compatibility
```

### Step 6: Migrate Tests
```typescript
// Move tests alongside the migrated code
// Before: packages/web/src/utils/validateMove.test.ts
// After: packages/core/src/domain/service/validateMove.test.ts

// Update test imports
import { validateMove } from "../validateMove";
import { createMockBoard } from "../../test-utils";
```

## Example: Timer Service Migration

### Before (Web Package)
```typescript
// packages/web/src/services/timerService.ts
import { useGameStore } from "@/stores/gameStore";

export class TimerService {
    private interval: NodeJS.Timeout | null = null;
    
    startTimer(player: Player) {
        this.interval = setInterval(() => {
            useGameStore.getState().decrementTime(player);
        }, 1000);
    }
}
```

### After (Core Package)
```typescript
// packages/core/src/domain/service/timerService.ts
export interface TimerCallbacks {
    onTick: (player: Player, remainingTime: number) => void;
    onTimeout: (player: Player) => void;
}

export class TimerService {
    private callbacks: TimerCallbacks;
    
    constructor(callbacks: TimerCallbacks) {
        this.callbacks = callbacks;
    }
    
    tick(player: Player, currentTime: number): number {
        const newTime = currentTime - 1;
        
        if (newTime <= 0) {
            this.callbacks.onTimeout(player);
            return 0;
        }
        
        this.callbacks.onTick(player, newTime);
        return newTime;
    }
}
```

### Web Package Adapter
```typescript
// packages/web/src/adapters/timerAdapter.ts
import { TimerService } from "shogi-core";
import { useGameStore } from "@/stores/gameStore";

export function createTimerService(): TimerService {
    return new TimerService({
        onTick: (player, time) => {
            useGameStore.getState().updateTime(player, time);
        },
        onTimeout: (player) => {
            useGameStore.getState().handleTimeout(player);
        }
    });
}
```

## Common Patterns

### 1. Dependency Injection
```typescript
// Core: Define interfaces for external dependencies
export interface RandomGenerator {
    next(): number;
}

export function shufflePieces(pieces: Piece[], random: RandomGenerator): Piece[] {
    // Use injected random generator
}

// Web: Provide implementation
const random: RandomGenerator = {
    next: () => Math.random()
};

const shuffled = shufflePieces(pieces, random);
```

### 2. Callback Pattern
```typescript
// Core: Use callbacks for side effects
export interface MoveValidator {
    validate(move: Move): ValidationResult;
    onInvalidMove?: (reason: string) => void;
}

// Web: Implement UI feedback
const validator: MoveValidator = {
    validate: validateMove,
    onInvalidMove: (reason) => {
        toast.error(`Invalid move: ${reason}`);
    }
};
```

### 3. Event Emitter Pattern
```typescript
// Core: Define event types
export type GameEvent = 
    | { type: "move"; move: Move }
    | { type: "checkmate"; winner: Player }
    | { type: "draw"; reason: string };

export class GameEngine extends EventEmitter<GameEvent> {
    makeMove(move: Move) {
        // ... apply move
        this.emit({ type: "move", move });
    }
}

// Web: Subscribe to events
gameEngine.on("checkmate", (event) => {
    showWinnerDialog(event.winner);
});
```

## Testing Strategy

### Core Package Tests
```typescript
// Pure unit tests with no mocks
describe("validateMove", () => {
    it("should validate legal moves", () => {
        const board = createBoard();
        const move = { from: "77", to: "76" };
        
        expect(validateMove(board, move)).toBe(true);
    });
});
```

### Web Package Tests
```typescript
// Integration tests with mocks
describe("GameComponent", () => {
    it("should use core validation", () => {
        const validateMoveSpy = vi.spyOn(shogiCore, "validateMove");
        
        render(<Game />);
        // ... interact with component
        
        expect(validateMoveSpy).toHaveBeenCalled();
    });
});
```

## Migration Checklist

### Pre-Migration
- [ ] Identify all dependencies
- [ ] Check for UI/browser dependencies
- [ ] Plan the module structure
- [ ] Consider backward compatibility

### During Migration
- [ ] Move types first
- [ ] Migrate pure functions
- [ ] Update imports incrementally
- [ ] Migrate tests alongside code
- [ ] Update documentation

### Post-Migration
- [ ] Run all tests
- [ ] Update import statements
- [ ] Remove old code
- [ ] Update documentation
- [ ] Check bundle size

## Common Pitfalls

### 1. Circular Dependencies
```typescript
// ❌ Bad: Circular dependency
// core/src/game.ts
import { Board } from "./board";

// core/src/board.ts
import { Game } from "./game";

// ✅ Good: Use interfaces
// core/src/types.ts
export interface IGame { ... }
export interface IBoard { ... }
```

### 2. Hidden Browser Dependencies
```typescript
// ❌ Bad: Uses browser API
export function saveGame(state: GameState) {
    localStorage.setItem("game", JSON.stringify(state));
}

// ✅ Good: Return serializable data
export function serializeGame(state: GameState): string {
    return JSON.stringify(state);
}
```

### 3. State Management Coupling
```typescript
// ❌ Bad: Coupled to Zustand
import { useGameStore } from "@/stores/gameStore";

export function makeMove(move: Move) {
    useGameStore.getState().applyMove(move);
}

// ✅ Good: Pure function
export function applyMove(board: Board, move: Move): Board {
    // Return new board state
}
```

## Benefits of Core Migration

1. **Reusability**: Use in different environments
2. **Testability**: Easier to test pure functions
3. **Performance**: Tree-shaking unused code
4. **Maintainability**: Clear separation of concerns
5. **Type Safety**: Shared type definitions