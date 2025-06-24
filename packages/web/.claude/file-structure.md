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
├── CapturedPieces.tsx     # Captured pieces display
├── GameControls.tsx       # Game control buttons (new game, etc.)
├── MoveHistory.tsx        # Move history display
├── Timer.tsx              # Timer component with countdown logic
├── TimerDisplay.tsx       # Timer visual display
├── PlaybackControls.tsx   # Game playback controls
├── PromotionDialog.tsx    # Piece promotion dialog
├── ResetDialog.tsx        # Game reset confirmation dialog
├── TimerSettingsDialog.tsx # Timer settings configuration
├── KifuExportDialog.tsx   # Export game to Kifu format
├── KifuImportDialog.tsx   # Import game from Kifu format
├── KeyboardHelp.tsx       # Keyboard shortcuts help
└── AudioTestPanel.tsx     # Audio system test panel (debug)
```

### UI Components (shadcn/ui)
```
src/components/ui/
├── button.tsx             # Button component
├── card.tsx               # Card component
├── input.tsx              # Input component
├── table.tsx              # Table component
├── alert.tsx              # Alert component
├── alert-dialog.tsx       # Alert dialog component
├── dialog.tsx             # Dialog component
├── label.tsx              # Label component
├── checkbox.tsx           # Checkbox component
├── radio-group.tsx        # Radio group component
├── select.tsx             # Select dropdown component
└── textarea.tsx           # Textarea component
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

### Hooks
```
src/hooks/
├── useAudio.ts            # Audio playback hook
├── useGameSettings.ts     # Game settings management
├── useKeyboardShortcuts.ts # Keyboard shortcuts handler
└── useLocalStorage.ts     # Local storage persistence
```

### Services
```
src/services/
├── audioManager.ts        # Audio playback management
├── audioGenerator.ts      # Dynamic audio generation
└── audioLogger.ts         # Audio debug logging
```

### Type Definitions
```
src/types/
├── audio.ts               # Audio-related types
├── audioConfig.ts         # Audio configuration types
├── audioErrors.ts         # Audio error types
├── timer.ts               # Timer-related types
├── settings.ts            # Game settings types
├── result.ts              # Result wrapper type
└── ui.ts                  # UI component types
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
import { Dialog, DialogContent } from "@/components/ui/dialog";
```

### Hooks
```typescript
import { useAudio } from "@/hooks/useAudio";
import { useGameSettings } from "@/hooks/useGameSettings";
import { useKeyboardShortcuts } from "@/hooks/useKeyboardShortcuts";
```

### Services
```typescript
import { audioManager } from "@/services/audioManager";
import { generateAudio } from "@/services/audioGenerator";
```

### Types
```typescript
import type { SoundType, AudioConfig } from "@/types/audio";
import type { TimerState, TimerConfig } from "@/types/timer";
import type { GameSettings } from "@/types/settings";
```

## Common File Locations

- **Game logic**: Imported from "shogi-core" package
- **UI components**: `src/components/` and `src/components/ui/`
- **State management**: `src/stores/gameStore.ts`
- **Hooks**: `src/hooks/` - Custom React hooks
- **Services**: `src/services/` - Business logic services
- **Types**: `src/types/` - TypeScript type definitions
- **Tests**: Co-located with source files (*.test.ts, *.test.tsx)
- **Stories**: Co-located with components (*.stories.tsx)
- **Utilities**: `src/lib/utils.ts`