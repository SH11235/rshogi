# Opening Book Debug Logs

This file documents the debug logs added to help diagnose the opening book issue when the player plays first.

## Debug Points Added

1. **gameStore.ts (line 2707)**
   - Logs moveHistory length being passed to AI
   - `[executeAIMove] Passing moveHistory to AI: {length}`

2. **aiService.ts (line 150-152)**
   - Logs moveHistory length being sent to worker
   - `[AIService] Sending calculate_move with moveHistory: {length}`

3. **aiWorker.ts (line 105-108)**
   - Logs moveHistory length received in worker
   - `[Worker] calculateBestMove called with moveHistory: {length}`

4. **engine.ts (line 120)**
   - Logs moveHistory length received by AI engine
   - `[AIEngine] calculateBestMove called with moveHistory: {length}`

5. **wasmOpeningBookLoader.ts (existing logs)**
   - Line 230-234: Logs moveHistory length and currentPlayer
   - Line 238-239: Logs calculated move number
   - Line 247-248: Logs generated SFEN and turn indicator
   - Line 283-284: Tests initial position for comparison
   - Line 287-291: Logs WASM find_moves results

## Expected Behavior

When the player plays first (e.g., 7g7f):
1. moveHistory should have length 1
2. currentPlayer should be "white" (AI's turn)
3. SFEN should reflect the player's move
4. Opening book should find appropriate responses

## How to Test

1. Start a new game with player as black (first)
2. Make a move (e.g., 7g7f)
3. Check console logs for the debug messages
4. Verify that moveHistory length progresses correctly through each layer

## Fix Applied

Added `moveHistory?: Move[]` to `FindMovesOptions` interface in `openingBook.ts` to properly support passing move history to the opening book.