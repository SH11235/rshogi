# Quick Reference Guide

Essential information for common development tasks to minimize context reading.

## Immediate Checks After Code Changes

```bash
# Run these 4 commands in order after any code changes:
npm run build      # Must pass - build check
npm run typecheck  # Must pass - type validation  
npm run lint       # Must pass - code quality
npm run format:check # Auto-formats code
```

## Test Status
- **Total Tests**: 83 tests across 8 files
- **All Passing**: Yes ✅
- **Coverage**: Components, stores, utilities, UI components

## Current Dependencies

### Core Dependencies
- React 18 with TypeScript
- Vite for bundling
- Tailwind CSS for styling
- Zustand for state management

### UI & Testing
- shadcn/ui + Radix UI primitives
- Vitest + @testing-library/react
- Storybook for component development
- Biome for linting/formatting

## File Creation Guidelines

### New Component
```typescript
// src/components/NewComponent.tsx
import { useGameStore } from "@/stores/gameStore";

interface NewComponentProps {
    // Props with explicit types
}

export function NewComponent({ }: NewComponentProps) {
    return <div />;
}
```

### New Test File
```typescript
// src/components/NewComponent.test.tsx
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { NewComponent } from "./NewComponent";

describe("NewComponent", () => {
    it("renders correctly", () => {
        render(<NewComponent />);
        expect(screen.getByRole("...")).toBeInTheDocument();
    });
});
```

### New Story File
```typescript
// src/components/NewComponent.stories.tsx
import type { Meta, StoryObj } from "@storybook/react";
import { NewComponent } from "./NewComponent";

const meta: Meta<typeof NewComponent> = {
    title: "Components/NewComponent",
    component: NewComponent,
};

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {
    args: {},
};
```

## Common Import Paths

```typescript
// Game logic from core package
import { type Board, createPiece } from "shogi-core";

// State management
import { useGameStore } from "@/stores/gameStore";

// UI components
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";

// Game components
import { Board } from "@/components/Board";
import { Piece } from "@/components/Piece";

// Utilities
import { cn } from "@/lib/utils";

// Testing utilities (global, no imports needed)
// describe, it, expect, vi, beforeEach
```

## Game State Quick Access

```typescript
// Get entire state
const state = useGameStore.getState();

// Common actions
const { selectSquare, makeMove, resetGame } = useGameStore();

// Subscribe to specific state
const currentPlayer = useGameStore((state) => state.currentPlayer);
const board = useGameStore((state) => state.board);
```

## Development Workflow

1. **Start development**: `npm run dev`
2. **Component development**: `npm run storybook`
3. **Run tests**: `npm run test` or `npm run test:watch`
4. **Check code quality**: Run the 4 validation commands
5. **Ready for commit**: All checks must pass

## Prohibited Patterns

- ❌ Using `any` type anywhere
- ❌ Using `forEach` in test files (use for-of)
- ❌ Missing `type="button"` on buttons
- ❌ Importing test utilities (they're global)
- ❌ Creating files without tests when adding new components

## Emergency Fixes

### Build Failing
1. Check TypeScript errors: `npm run typecheck`
2. Check import paths and missing exports
3. Verify core package types are properly imported

### Tests Failing
1. Check if imports are correct
2. Verify mock functions are cleared in beforeEach
3. Ensure proper type guards for union types

### Lint Errors
1. Convert forEach to for-of loops
2. Remove unused imports
3. Add missing button types
4. Fix formatting with `npm run format:check`