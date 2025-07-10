# State Management Patterns with Zustand

This document describes state management patterns and best practices using Zustand in the Shogi application.

## Table of Contents
1. [Store Architecture](#store-architecture)
2. [State Organization](#state-organization)
3. [Complex State Updates](#complex-state-updates)
4. [Online Game State](#online-game-state)
5. [History Management](#history-management)
6. [Performance Optimization](#performance-optimization)
7. [Testing Patterns](#testing-patterns)

## Store Architecture

### Single Store Pattern
We use a single store pattern for the game state to ensure consistency and simplify state management.

```typescript
interface GameState {
    // Game state
    board: Board;
    hands: Hands;
    currentPlayer: Player;
    gameStatus: GameStatus;
    
    // UI state
    selectedSquare: Square | null;
    validMoves: Square[];
    selectedDropPiece: { pieceType: PieceType; player: Player } | null;
    
    // History
    moveHistory: Move[];
    historyCursor: number;
    
    // Online game
    isOnlineGame: boolean;
    connectionStatus: ConnectionStatus;
    localPlayer: Player | null;
    
    // Actions
    selectSquare: (square: Square) => void;
    makeMove: (from: Square, to: Square, promote?: boolean) => void;
    // ... more actions
}
```

### Slice Pattern for Complex Features
```typescript
// Timer slice
interface TimerSlice {
    timer: TimerState & TimerActions;
}

// Combine slices
type GameStore = GameState & TimerSlice;
```

## State Organization

### Separation of Concerns
1. **Game Logic**: Board, pieces, moves, rules
2. **UI State**: Selection, highlights, dialogs
3. **Network State**: Connection, messages, sync
4. **Meta State**: Settings, preferences, history

### State Shape Design
```typescript
// Flat structure for easy updates
const state = {
    board: { '11': piece1, '12': piece2, ... },
    hands: { black: { '歩': 2 }, white: { '歩': 1 } }
};

// Avoid deeply nested structures
// Bad: state.game.board.squares[0][0].piece
// Good: state.board['11']
```

## Complex State Updates

### Atomic Updates
```typescript
makeMove: (from: Square, to: Square, promote?: boolean) => {
    const state = get();
    
    // Validate move first
    if (!isValidMove(state, from, to)) return;
    
    // Apply all changes atomically
    set((state) => {
        const result = applyMove(state.board, state.hands, state.currentPlayer, move);
        
        return {
            board: result.board,
            hands: result.hands,
            currentPlayer: result.nextTurn,
            selectedSquare: null,
            validMoves: [],
            moveHistory: [...state.moveHistory, move],
            gameStatus: calculateGameStatus(result.board, result.nextTurn)
        };
    });
}
```

### Transaction Pattern
```typescript
// For complex multi-step updates
const executeGameAction = (action: GameAction) => {
    const rollbackState = get();
    
    try {
        // Step 1: Update local state
        applyLocalUpdate(action);
        
        // Step 2: Send to remote
        if (isOnlineGame) {
            sendMessage(action);
        }
        
        // Step 3: Update UI
        updateUIState(action);
        
    } catch (error) {
        // Rollback on error
        set(rollbackState);
        throw error;
    }
};
```

## Online Game State

### Connection Management
```typescript
interface ConnectionState {
    isOnlineGame: boolean;
    connectionStatus: ConnectionStatus;
    webrtcConnection: WebRTCConnection | null;
    localPlayer: Player | null;
    
    // Connection actions
    startOnlineGame: (isHost: boolean) => Promise<string>;
    joinOnlineGame: (offer: string) => Promise<string>;
    handleOnlineMessage: (message: GameMessage) => void;
    disconnectOnline: () => void;
}
```

### Message Handling Pattern
```typescript
handleOnlineMessage: (message: GameMessage) => {
    // Type-safe message handling
    if (isMoveMessage(message)) {
        handleMoveMessage(message);
    } else if (isTimerUpdateMessage(message)) {
        handleTimerMessage(message);
    } else if (isDrawOfferMessage(message)) {
        handleDrawOfferMessage(message);
    }
    // ... handle all message types
}
```

### State Synchronization
```typescript
// Optimistic updates
const makeOnlineMove = (move: Move) => {
    // 1. Apply locally immediately
    applyMove(move);
    
    // 2. Send to peer
    sendMessage({ type: 'move', data: move });
    
    // 3. Handle confirmation/rejection
    // If rejected, rollback the move
};

```

## History Management

### Time Travel Implementation
```typescript
interface HistoryState {
    moveHistory: Move[];
    historyCursor: number;
    branchInfo: BranchInfo | null;
    
    gameUndo: () => void;
    gameRedo: () => void;
    goToMove: (moveIndex: number) => void;
}
```

### History Navigation
```typescript
gameUndo: () => {
    const { historyCursor, moveHistory } = get();
    
    if (historyCursor <= 0) return;
    
    const targetIndex = historyCursor - 1;
    const reconstructed = reconstructGameState(moveHistory, targetIndex);
    
    set({
        ...reconstructed,
        historyCursor: targetIndex,
        selectedSquare: null,
        validMoves: []
    });
}
```

### Branch Management
```typescript
// Handle branching when making moves from past positions
const makeMoveFromHistory = (move: Move) => {
    const { historyCursor, moveHistory } = get();
    
    if (historyCursor < moveHistory.length - 1) {
        // Create branch
        const branchPoint = historyCursor;
        const mainLine = moveHistory.slice(0, branchPoint + 1);
        const branchMoves = [move];
        
        set({
            moveHistory: [...mainLine, move],
            branchInfo: {
                branchPoint,
                mainLine: moveHistory,
                branchMoves
            }
        });
    }
};
```

## Performance Optimization

### Selective Updates
```typescript
// Use selectors to prevent unnecessary re-renders
const useBoard = () => useGameStore((state) => state.board);
const useCurrentPlayer = () => useGameStore((state) => state.currentPlayer);

// Combine related state
const useGameInfo = () => useGameStore((state) => ({
    currentPlayer: state.currentPlayer,
    gameStatus: state.gameStatus,
    moveCount: state.moveHistory.length
}));
```

### Memoization
```typescript
// Memoize expensive computations
const useValidMoves = (square: Square) => {
    return useGameStore((state) => {
        if (!square) return [];
        return useMemo(
            () => calculateValidMoves(state.board, square),
            [state.board, square]
        );
    });
};
```

### Shallow Comparison
```typescript
// Use shallow for performance
const useShallowGameState = () => {
    return useGameStore(
        (state) => ({
            board: state.board,
            currentPlayer: state.currentPlayer
        }),
        shallow
    );
};
```

## Testing Patterns

### Store Testing
```typescript
describe('gameStore', () => {
    beforeEach(() => {
        // Reset to initial state
        useGameStore.setState(initialGameState);
    });
    
    it('should update state correctly', () => {
        const { makeMove } = useGameStore.getState();
        
        act(() => {
            makeMove({ row: 7, column: 7 }, { row: 7, column: 6 });
        });
        
        const state = useGameStore.getState();
        expect(state.currentPlayer).toBe('white');
        expect(state.board['76']).toBeDefined();
    });
});
```

### Mocking Store
```typescript
// Mock specific parts of the store
const mockGameStore = (initialState?: Partial<GameState>) => {
    const state = {
        ...defaultGameState,
        ...initialState
    };
    
    useGameStore.setState(state);
    
    return {
        ...state,
        // Mock actions
        makeMove: vi.fn(),
        selectSquare: vi.fn()
    };
};
```

### Integration Testing
```typescript
it('should handle complete game flow', () => {
    const TestComponent = () => {
        const { board, makeMove } = useGameStore();
        
        return (
            <div>
                <Board board={board} onSquareClick={makeMove} />
                <GameInfo />
            </div>
        );
    };
    
    render(<TestComponent />);
    
    // Test interactions
    fireEvent.click(screen.getByTestId('square-77'));
    fireEvent.click(screen.getByTestId('square-76'));
    
    // Verify state updates
    expect(screen.getByText('白の手番')).toBeInTheDocument();
});
```

## Best Practices

### Do's
- ✅ Keep state flat and normalized
- ✅ Use TypeScript for type safety
- ✅ Separate UI state from domain state
- ✅ Use selectors to optimize re-renders
- ✅ Handle errors gracefully

### Don'ts
- ❌ Don't mutate state directly
- ❌ Don't store derived state
- ❌ Don't mix async logic in reducers
- ❌ Don't create circular dependencies
- ❌ Don't store non-serializable values

## Common Patterns

### Loading States
```typescript
interface LoadingState {
    isLoading: boolean;
    error: Error | null;
    
    setLoading: (loading: boolean) => void;
    setError: (error: Error | null) => void;
}

const handleAsyncAction = async () => {
    set({ isLoading: true, error: null });
    
    try {
        const result = await asyncOperation();
        set({ data: result });
    } catch (error) {
        set({ error });
    } finally {
        set({ isLoading: false });
    }
};
```

### Middleware Pattern
```typescript
// Logging middleware
const logMiddleware = (config) => (set, get, api) =>
    config(
        (args) => {
            console.log('Previous state:', get());
            set(args);
            console.log('New state:', get());
        },
        get,
        api
    );

// Apply middleware
const useGameStore = create(
    logMiddleware((set, get) => ({
        // ... store definition
    }))
);
```

### Persistence
```typescript
// Persist game state
const useGameStore = create(
    persist(
        (set, get) => ({
            // ... store definition
        }),
        {
            name: 'shogi-game-state',
            partialize: (state) => ({
                // Only persist specific fields
                moveHistory: state.moveHistory,
                settings: state.settings
            })
        }
    )
);
```