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

## Timer Integration Patterns

### Timer Hook Usage
```typescript
import { useGameStore } from "@/stores/gameStore";

// In component
const { timer, startPlayerTimer, switchTimer, pauseTimer } = useGameStore();

// Start timer for current player
if (timer.config.mode) {
    startPlayerTimer(currentPlayer);
}

// Switch timer on move
const handleMove = () => {
    makeMove(from, to);
    if (timer.config.mode) {
        switchTimer();
    }
};

// Format timer display
const formatTime = (ms: number): string => {
    const seconds = Math.ceil(ms / 1000);
    const minutes = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${minutes}:${secs.toString().padStart(2, "0")}`;
};
```

### Timer Settings Pattern
```typescript
import { TimerSettingsDialog } from "@/components/TimerSettingsDialog";
import type { TimerConfig } from "@/types/timer";

const [timerConfig, setTimerConfig] = useState<TimerConfig>({
    mode: "basic",
    basicTime: 600,
    byoyomiTime: 30,
    fischerIncrement: 0,
    perMoveLimit: 0,
});

<TimerSettingsDialog
    config={timerConfig}
    onSave={(config) => {
        setTimerConfig(config);
        initializeTimer(config);
    }}
/>
```

## Audio Patterns

### Audio Hook Usage
```typescript
import { useAudio } from "@/hooks/useAudio";
import { useGameSettings } from "@/hooks/useGameSettings";

// In component
const audio = useAudio();
const { settings } = useGameSettings();

// Play sound on move
const handleMove = async () => {
    const result = makeMove(from, to);
    if (result.captured && settings.audio.pieceSound) {
        await audio.play("piece");
    }
    if (result.isCheck && settings.audio.checkSound) {
        await audio.play("check");
    }
};

// Game end sound
if (gameStatus === "checkmate" && settings.audio.gameEndSound) {
    audio.play("gameEnd");
}
```

### Audio Settings Pattern
```typescript
import type { AudioSettings } from "@/types/settings";

const [audioSettings, setAudioSettings] = useState<AudioSettings>({
    masterVolume: 50,
    pieceSound: true,
    checkSound: true,
    gameEndSound: true,
});

// Volume control
<input
    type="range"
    min={0}
    max={100}
    step={25}
    value={audioSettings.masterVolume}
    onChange={(e) => {
        const volume = Number(e.target.value) as VolumeLevel;
        setAudioSettings(prev => ({ ...prev, masterVolume: volume }));
        audio.setVolume(volume);
    }}
/>
```

## Dialog Component Patterns

### Dialog Usage Pattern
```typescript
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "@/components/ui/dialog";

const [isOpen, setIsOpen] = useState(false);

<Dialog open={isOpen} onOpenChange={setIsOpen}>
    <DialogContent>
        <DialogHeader>
            <DialogTitle>Dialog Title</DialogTitle>
            <DialogDescription>
                Dialog description text
            </DialogDescription>
        </DialogHeader>
        <div className="py-4">
            {/* Dialog body content */}
        </div>
        <DialogFooter>
            <Button variant="outline" onClick={() => setIsOpen(false)}>
                Cancel
            </Button>
            <Button onClick={handleConfirm}>
                Confirm
            </Button>
        </DialogFooter>
    </DialogContent>
</Dialog>
```

### Alert Dialog Pattern
```typescript
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
} from "@/components/ui/alert-dialog";

<AlertDialog open={showReset} onOpenChange={setShowReset}>
    <AlertDialogContent>
        <AlertDialogHeader>
            <AlertDialogTitle>Are you sure?</AlertDialogTitle>
            <AlertDialogDescription>
                This will reset the game and cannot be undone.
            </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={handleReset}>
                Reset Game
            </AlertDialogAction>
        </AlertDialogFooter>
    </AlertDialogContent>
</AlertDialog>
```

## Keyboard Shortcuts Pattern

### useKeyboardShortcuts Hook
```typescript
import { useKeyboardShortcuts } from "@/hooks/useKeyboardShortcuts";
import { useCallback } from "react";

// Define shortcuts
const shortcuts = {
    "cmd+z": handleUndo,
    "cmd+shift+z": handleRedo,
    "r": () => setShowReset(true),
    "h": () => setShowHelp(true),
    "escape": handleEscape,
};

// Use the hook
useKeyboardShortcuts(shortcuts);

// Handle escape for dialogs
const handleEscape = useCallback(() => {
    if (promotionPending) {
        cancelPromotion();
    } else if (selectedSquare) {
        deselectAll();
    }
}, [promotionPending, selectedSquare]);
```

### Keyboard Help Component
```typescript
<KeyboardHelp
    shortcuts={[
        { key: "Cmd/Ctrl + Z", description: "Undo move" },
        { key: "Cmd/Ctrl + Shift + Z", description: "Redo move" },
        { key: "R", description: "Reset game" },
        { key: "H", description: "Show help" },
        { key: "Escape", description: "Cancel selection" },
    ]}
/>
```