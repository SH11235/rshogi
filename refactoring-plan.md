# Refactoring Plan: Moving Game Logic to Core Packages

## Overview
This document identifies game logic currently in packages/web that should be moved to packages/core or rust-core for better separation of concerns.

## Logic to Move to packages/core

### 1. Game State Reconstruction Logic
**Current Location**: `packages/web/src/stores/gameStore.ts` (lines 62-97)
**Function**: `reconstructGameStateWithInitial`
**Why Move**: This is pure game logic that reconstructs board state from move history. It has no UI dependencies.
**Suggested Location**: `packages/core/src/domain/service/gameStateService.ts`

### 2. Drop Move Validation
**Current Location**: `packages/web/src/stores/gameStore.ts` (lines 100-108)
**Function**: `getValidDropSquares`
**Why Move**: While it currently delegates to core, this wrapper function could be eliminated.

### 3. Timer Logic
**Current Location**: `packages/web/src/types/timer.ts`
**Functions**: 
- `getWarningLevel` (lines 146-151)
- `formatByoyomiTime` (lines 139-143)
- Timer state calculations
**Why Move**: Timer management is game logic, not UI logic. Only formatting functions should remain in web.
**Suggested Location**: `packages/core/src/domain/service/timerService.ts`

### 4. Move Validation Logic
**Current Location**: `packages/web/src/utils/validateMove.ts`
**Functions**: All validation functions
**Why Move**: Move validation is core game logic that should be centralized.
**Suggested Location**: `packages/core/src/domain/service/moveValidationService.ts`

### 5. Game Mode and Branch Management
**Current Location**: `packages/web/src/stores/gameStore.ts`
**Types/Logic**:
- `BranchInfo` type (lines 120-126)
- `ReviewBasePosition` type (lines 128-134)
- Branch creation logic (lines 498-533, 632-666)
**Why Move**: Game variation/branch management is domain logic.
**Suggested Location**: `packages/core/src/domain/model/gameVariation.ts`

### 6. Game Status Determination
**Current Location**: `packages/web/src/stores/gameStore.ts` (lines 486-496, 618-629)
**Logic**: Determining checkmate, check, and win conditions
**Why Move**: Already partially in core, but the win determination logic should be centralized.

## Logic to Move to rust-core

### 1. Performance-Critical Move Generation
**Consider Moving**: Legal move generation for complex positions
**Why**: Rust/WASM can provide significant performance improvements for move generation in complex positions.

### 2. Game State Evaluation
**Consider Moving**: Position evaluation for analysis features
**Why**: CPU-intensive calculations benefit from Rust's performance.

### 3. Perft Testing
**Consider Moving**: Move generation verification
**Why**: Performance testing of move generation is ideal for Rust.

## Logic That Should Stay in packages/web

### 1. UI State Management
- Selected square/piece tracking
- Valid move highlighting
- Promotion dialog state
- Animation states

### 2. Audio Management
- Sound effect triggers
- Volume controls
- Audio initialization

### 3. Online Game Communication
- WebRTC connection management
- Message handling for multiplayer
- Connection status tracking

### 4. User Preferences
- Theme settings
- Display options
- Local storage management

### 5. Keyboard Shortcuts
- Input handling
- Command mapping

## Refactoring Steps

### Phase 1: Move Pure Game Logic (Priority: High)
1. Move `reconstructGameStateWithInitial` to core
2. Move move validation functions to core
3. Create proper timer service in core

### Phase 2: Consolidate Game State Management (Priority: Medium)
1. Move branch/variation management to core
2. Centralize game status determination
3. Create unified game state service

### Phase 3: Performance Optimization (Priority: Low)
1. Evaluate which algorithms benefit from Rust
2. Implement critical paths in rust-core
3. Create benchmarks to measure improvements

## Benefits of Refactoring

1. **Better Separation of Concerns**: UI logic separated from game logic
2. **Reusability**: Core logic can be used in other clients (mobile, CLI, etc.)
3. **Testability**: Pure functions are easier to test
4. **Performance**: Critical paths can be optimized in Rust
5. **Maintainability**: Clear boundaries between packages

## Implementation Notes

- Maintain backward compatibility during refactoring
- Add comprehensive tests before moving code
- Use TypeScript interfaces to define clear contracts
- Consider using a facade pattern in web to minimize breaking changes