# Common Patterns Reference

Frequently used code patterns to reduce token usage and ensure consistency.

## Testing Patterns

### Component Test Setup
```typescript
import { render, screen, fireEvent } from "@testing-library/react";
import { describe, expect, it, vi, beforeEach } from "vitest";

describe("ComponentName", () => {
    const mockFunction = vi.fn();
    
    beforeEach(() => {
        mockFunction.mockClear();
    });
    
    it("renders correctly", () => {
        render(<Component prop={value} />);
        expect(screen.getByText("text")).toBeInTheDocument();
    });
});
```

### Store Test Pattern
```typescript
import { useGameStore } from "./gameStore";

describe("useGameStore", () => {
    beforeEach(() => {
        useGameStore.getState().resetGame();
    });
    
    it("should have correct initial state", () => {
        const state = useGameStore.getState();
        expect(state.currentPlayer).toBe("black");
    });
});
```

### Type-Safe Test Helpers
```typescript
// Avoid "any" - use proper types
const createEmptyBoard = (): Board => {
    const board: Partial<Board> = {};
    for (let row = 1; row <= 9; row++) {
        for (let col = 1; col <= 9; col++) {
            board[`${row}${col}` as keyof Board] = null;
        }
    }
    return board as Board;
};

// Type guards for union types
const move = state.moveHistory[0];
if (move.type === "move") {
    expect(move.from).toEqual(expectedSquare);
}
```

## Component Patterns

### Game Component Structure
```typescript
import { useGameStore } from "@/stores/gameStore";

export function ComponentName() {
    const { state, actions } = useGameStore();
    
    const handleAction = (param: Type) => {
        actions.methodName(param);
    };
    
    return (
        <div className="container">
            {/* JSX */}
        </div>
    );
}
```

### Square/Piece Interaction
```typescript
// Square key generation
const squareKey = `${square.row}${square.column}` as keyof Board;

// Square click handling
const handleSquareClick = (square: Square) => {
    selectSquare(square);
};

// Piece rendering with Japanese characters
const pieceDisplay = piece.promoted 
    ? getPromotedDisplay(piece.type)
    : getBaseDisplay(piece.type);
```

## State Management Patterns

### Zustand Store Pattern
```typescript
interface GameState {
    // State properties
    property: Type;
    
    // Actions
    actionName: (param: Type) => void;
}

export const useGameStore = create<GameState>()(
    persist(
        (set, get) => ({
            // Initial state
            property: initialValue,
            
            // Actions
            actionName: (param) => set((state) => ({
                ...state,
                property: newValue
            })),
        }),
        {
            name: "game-store",
        }
    )
);
```

## UI Component Patterns

### shadcn/ui Component Usage
```typescript
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

// Button with proper type
<Button type="button" variant="outline" onClick={handleClick}>
    Action
</Button>

// Card structure
<Card>
    <CardHeader>
        <CardTitle>Title</CardTitle>
    </CardHeader>
    <CardContent>
        Content
    </CardContent>
</Card>
```

### Styling Patterns
```typescript
import { cn } from "@/lib/utils";

// Conditional classes
const className = cn(
    "base-classes",
    condition && "conditional-classes",
    variant === "primary" && "variant-classes"
);

// Grid layouts for board
<div className="grid grid-cols-9 gap-0">
    {squares.map((square) => (
        <div key={square.key} className="aspect-square">
            {/* Square content */}
        </div>
    ))}
</div>
```

## Error Prevention Patterns

### Avoid "any" Type
```typescript
// ❌ Don't use any
const value: any = getData();

// ✅ Use proper types
const value: ExpectedType = getData();

// ✅ Use type guards
if (value.type === "expected") {
    // TypeScript knows the type here
}

// ✅ Use type assertions carefully
const typedValue = value as ExpectedType;
```

### Prefer for-of over forEach
```typescript
// ❌ Avoid forEach in tests
items.forEach((item) => {
    expect(item).toBeDefined();
});

// ✅ Use for-of
for (const item of items) {
    expect(item).toBeDefined();
}
```

### Required Props
```typescript
// Always specify button type
<button type="button" onClick={handler}>
    Click me
</button>

// Use proper event types
const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    // Handle click
};
```