# File Structure Reference

This document provides a quick reference for the current file structure to reduce token usage when asking about specific files.

## Key Files by Category

### Core Application
```
src/
├── App.tsx                 # Main app component
├── main.tsx               # Application entry point
└── vite-env.d.ts         # Vite type definitions
```

### Game Components
```
src/components/
├── Board.tsx              # 9x9 shogi board (main game UI)
├── Piece.tsx              # Individual piece rendering
├── GameInfo.tsx           # Game status and controls
└── CapturedPieces.tsx     # Captured pieces display
```

### UI Components (shadcn/ui)
```
src/components/ui/
├── button.tsx             # Button component
├── card.tsx               # Card component
├── input.tsx              # Input component
└── table.tsx              # Table component
```

### State Management
```
src/stores/
└── gameStore.ts           # Main Zustand game state store
```

### Utilities
```
src/lib/
└── utils.ts               # Utility functions (cn, etc.)
```

### Testing
```
src/test/
└── setup.ts               # Test configuration

# Test files are co-located with source files:
src/components/*.test.tsx   # Component tests
src/stores/*.test.ts        # Store tests
src/lib/*.test.ts          # Utility tests
```

### Storybook Stories
```
src/components/*.stories.tsx  # Component stories
```

### Configuration Files
```
├── package.json           # Dependencies and scripts
├── vite.config.ts         # Vite configuration
├── vitest.config.ts       # Test configuration
├── tsconfig.json          # TypeScript configuration
├── tsconfig.app.json      # App-specific TS config
├── components.json        # shadcn/ui configuration
└── .storybook/            # Storybook configuration
```

## Import Patterns

### From Core Package
```typescript
import { type Board, type Piece, createPiece } from "shogi-core";
```

### Internal Components
```typescript
import { Board } from "@/components/Board";
import { useGameStore } from "@/stores/gameStore";
import { cn } from "@/lib/utils";
```

### UI Components
```typescript
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
```

## Common File Locations

- **Game logic**: Imported from "shogi-core" package
- **UI components**: `src/components/` and `src/components/ui/`
- **State management**: `src/stores/gameStore.ts`
- **Tests**: Co-located with source files
- **Stories**: Co-located with components
- **Utilities**: `src/lib/utils.ts`