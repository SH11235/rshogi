# Testing Strategies for Shogi Application

This document outlines comprehensive testing strategies used in the Shogi application, with a focus on WebRTC, async operations, and complex state management.

## Table of Contents
1. [Testing Philosophy](#testing-philosophy)
2. [Test Structure](#test-structure)
3. [WebRTC Testing](#webrtc-testing)
4. [Async Operations](#async-operations)
5. [State Management Testing](#state-management-testing)
6. [Component Testing](#component-testing)
7. [Integration Testing](#integration-testing)
8. [Common Patterns](#common-patterns)

## Testing Philosophy

Following Kent C. Dodds' Testing Trophy:
- **Static**: TypeScript, ESLint, Biome
- **Unit**: Core logic, utilities, services
- **Integration**: Component interactions, store updates
- **E2E**: Critical user flows (future implementation)

## Test Structure

### File Organization
```
src/
├── components/
│   ├── Board.tsx
│   └── Board.test.tsx
├── services/
│   ├── webrtc.ts
│   └── webrtc.test.ts
└── stores/
    ├── gameStore.ts
    └── gameStore.test.ts
```

### Test Setup
```typescript
// vitest.config.ts
import { defineConfig } from 'vitest/config';

export default defineConfig({
    test: {
        environment: 'happy-dom',
        globals: true,
        setupFiles: ['./src/test-utils/setup.ts'],
        coverage: {
            reporter: ['text', 'json', 'html'],
            exclude: ['**/*.stories.tsx', '**/*.test.{ts,tsx}']
        }
    }
});
```

## WebRTC Testing

### Mocking Strategy
```typescript
// Mock the entire SimpleWebRTC module
vi.mock('@/wasm/simple_webrtc', () => ({
    default: vi.fn(),
    SimpleWebRTCPeer: vi.fn().mockImplementation(() => ({
        create_offer: vi.fn().mockResolvedValue('mock-offer'),
        accept_offer: vi.fn().mockResolvedValue('mock-answer'),
        accept_answer: vi.fn().mockResolvedValue(undefined),
        send_message: vi.fn(),
        on_message: vi.fn(),
        on_state_change: vi.fn(),
        close: vi.fn()
    }))
}));
```

### Testing Connection Lifecycle
```typescript
describe('WebRTCConnection', () => {
    it('should create host connection', async () => {
        const connection = new WebRTCConnection();
        const offer = await connection.createHost();
        
        expect(offer).toBe('mock-offer');
        expect(connection.getConnectionInfo().isHost).toBe(true);
    });
    
    it('should handle connection state changes', async () => {
        const connection = new WebRTCConnection();
        const stateChanges: string[] = [];
        
        connection.onConnectionStateChange((state) => {
            stateChanges.push(state);
        });
        
        // Simulate state changes
        mockPeer.on_state_change.mock.calls[0][0]('Connected');
        
        expect(stateChanges).toContain('connected');
    });
});
```

### Testing Error Scenarios
```typescript
it('should handle disconnection gracefully', async () => {
    const connection = new WebRTCConnection();
    const errorHandler = vi.fn();
    
    connection.onError(errorHandler);
    await connection.createHost();
    
    // Simulate disconnection
    mockPeer.on_state_change.mock.calls[0][0]('Disconnected');
    
    // Advance timers for reconnection
    vi.advanceTimersByTime(2000);
    
    expect(mockPeer.create_offer).toHaveBeenCalledTimes(2);
});
```

### Testing Message Handling
```typescript
it('should validate and process messages', async () => {
    const connection = new WebRTCConnection();
    const messageHandler = vi.fn();
    
    connection.onMessage(messageHandler);
    await connection.createHost();
    
    // Simulate incoming message
    const message = {
        type: 'move',
        data: { from: '77', to: '76' },
        timestamp: Date.now(),
        playerId: 'test-id'
    };
    
    mockPeer.on_message.mock.calls[0][0](JSON.stringify(message));
    
    expect(messageHandler).toHaveBeenCalledWith(message);
});
```

## Async Operations

### Timer Testing
```typescript
describe('Timer functionality', () => {
    beforeEach(() => {
        vi.useFakeTimers();
    });
    
    afterEach(() => {
        vi.useRealTimers();
    });
    
    it('should countdown correctly', async () => {
        const { result } = renderHook(() => useTimer());
        
        act(() => {
            result.current.startTimer('black');
        });
        
        // Advance time by 1 second
        act(() => {
            vi.advanceTimersByTime(1000);
        });
        
        expect(result.current.blackTime).toBe(299); // 300 - 1
    });
});
```

### Promise Testing
```typescript
it('should handle async state updates', async () => {
    const { result } = renderHook(() => useGameStore());
    
    // Start async operation
    const promise = result.current.loadGameFromServer('game-id');
    
    // Wait for loading state
    await waitFor(() => {
        expect(result.current.isLoading).toBe(true);
    });
    
    // Resolve promise
    await promise;
    
    // Verify final state
    expect(result.current.isLoading).toBe(false);
    expect(result.current.gameData).toBeDefined();
});
```

### Cleanup Testing
```typescript
it('should cleanup on unmount', async () => {
    const { unmount } = render(<WebRTCGame />);
    
    // Verify connection is established
    expect(mockPeer.create_offer).toHaveBeenCalled();
    
    // Unmount component
    unmount();
    
    // Verify cleanup
    expect(mockPeer.close).toHaveBeenCalled();
});
```

## State Management Testing

### Zustand Store Testing
```typescript
describe('gameStore', () => {
    beforeEach(() => {
        // Reset store to initial state
        useGameStore.setState(initialGameState);
    });
    
    it('should handle move validation', () => {
        const { selectSquare, makeMove } = useGameStore.getState();
        
        // Select a piece
        act(() => selectSquare({ row: 7, column: 7 }));
        
        const { selectedSquare, validMoves } = useGameStore.getState();
        expect(selectedSquare).toEqual({ row: 7, column: 7 });
        expect(validMoves).toHaveLength(1);
        
        // Make a move
        act(() => makeMove({ row: 7, column: 7 }, { row: 7, column: 6 }));
        
        const { currentPlayer } = useGameStore.getState();
        expect(currentPlayer).toBe('white');
    });
});
```

### Complex State Updates
```typescript
it('should handle spectator mode state', () => {
    const store = useGameStore.getState();
    
    // Start spectator mode
    act(() => {
        store.startSpectatorMode('game-123');
    });
    
    expect(store.isSpectatorMode).toBe(true);
    expect(store.gameMode).toBe('review');
    
    // Handle spectator sync
    act(() => {
        store.handleOnlineMessage({
            type: 'spectator_sync',
            data: {
                board: mockBoard,
                hands: mockHands,
                currentPlayer: 'white'
            }
        });
    });
    
    expect(store.board).toEqual(mockBoard);
    expect(store.currentPlayer).toBe('white');
});
```

## Component Testing

### Render Testing
```typescript
describe('Board component', () => {
    it('should render 9x9 grid', () => {
        render(<Board />);
        
        const squares = screen.getAllByRole('button', { name: /square/i });
        expect(squares).toHaveLength(81);
    });
    
    it('should highlight valid moves', () => {
        const { rerender } = render(<Board />);
        
        // Select a piece
        const piece = screen.getByTestId('square-77');
        fireEvent.click(piece);
        
        // Check for highlighted squares
        const validSquare = screen.getByTestId('square-76');
        expect(validSquare).toHaveClass('bg-green-200');
    });
});
```

### Interaction Testing
```typescript
it('should handle piece movement', async () => {
    const user = userEvent.setup();
    render(<Game />);
    
    // Click on a piece
    const piece = screen.getByTestId('square-77');
    await user.click(piece);
    
    // Click on valid move
    const target = screen.getByTestId('square-76');
    await user.click(target);
    
    // Verify move was made
    await waitFor(() => {
        expect(screen.getByTestId('square-76')).toHaveTextContent('歩');
    });
});
```

## Integration Testing

### Full Game Flow
```typescript
it('should play a complete game', async () => {
    const { result } = renderHook(() => useGameStore());
    
    // Make several moves
    const moves = [
        { from: { row: 7, column: 7 }, to: { row: 7, column: 6 } },
        { from: { row: 3, column: 3 }, to: { row: 3, column: 4 } },
        // ... more moves
    ];
    
    for (const move of moves) {
        act(() => {
            result.current.selectSquare(move.from);
            result.current.makeMove(move.from, move.to);
        });
    }
    
    // Verify game state
    expect(result.current.moveHistory).toHaveLength(moves.length);
    expect(result.current.gameStatus).toBe('playing');
});
```

### Online Game Testing
```typescript
it('should synchronize moves in online game', async () => {
    const host = renderHook(() => useGameStore());
    const guest = renderHook(() => useGameStore());
    
    // Setup connections
    await act(async () => {
        const offer = await host.result.current.startOnlineGame(true);
        const answer = await guest.result.current.joinOnlineGame(offer);
        await host.result.current.acceptOnlineAnswer(answer);
    });
    
    // Make move on host
    act(() => {
        host.result.current.makeMove({ row: 7, column: 7 }, { row: 7, column: 6 });
    });
    
    // Verify move synchronized to guest
    await waitFor(() => {
        expect(guest.result.current.board['76']).toBeDefined();
    });
});
```

## Common Patterns

### Test Utilities
```typescript
// test-utils/board.ts
export function createMockBoard(): Board {
    return {
        '11': { type: 'lance', owner: 'white', promoted: false },
        // ... other pieces
    };
}

// test-utils/render.tsx
export function renderWithStore(component: ReactElement, initialState?: Partial<GameState>) {
    if (initialState) {
        useGameStore.setState(initialState);
    }
    return render(component);
}
```

### Assertion Helpers
```typescript
// Custom matchers
expect.extend({
    toBeValidMove(received: Square, board: Board, from: Square) {
        const validMoves = generateLegalMoves(board, from);
        const pass = validMoves.some(move => 
            move.row === received.row && move.column === received.column
        );
        
        return {
            pass,
            message: () => `Expected ${JSON.stringify(received)} to be a valid move from ${JSON.stringify(from)}`
        };
    }
});
```

### Snapshot Testing
```typescript
it('should match board layout snapshot', () => {
    const { container } = render(<Board />);
    expect(container.firstChild).toMatchSnapshot();
});
```

## Best Practices

### Do's
- ✅ Test behavior, not implementation
- ✅ Use proper async/await patterns
- ✅ Mock external dependencies
- ✅ Test error scenarios
- ✅ Clean up after tests

### Don'ts
- ❌ Don't test implementation details
- ❌ Don't use real network connections
- ❌ Don't leave timers running
- ❌ Don't test third-party libraries
- ❌ Don't write brittle selectors

## Debugging Tips

1. **Console Logs**: Use `screen.debug()` to see rendered output
2. **Async Issues**: Use `waitFor` for async state updates
3. **Timer Issues**: Always use fake timers for time-dependent tests
4. **State Issues**: Reset store state between tests
5. **Mock Issues**: Verify mock calls with `expect(mock).toHaveBeenCalledWith()`