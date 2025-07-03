# AI Engine Implementation Details

## Overview

The Shogi AI engine uses modern chess programming techniques adapted for Shogi, including iterative deepening, alpha-beta pruning, transposition tables, and sophisticated evaluation functions.

## Architecture

### Component Structure

```
packages/web/src/services/ai/
├── engine.ts         # Main AI engine interface
├── evaluation.ts     # Position evaluation functions
├── search.ts         # Search algorithms and optimizations
├── engine.test.ts    # Engine tests
├── evaluation.test.ts # Evaluation tests
└── search.test.ts    # Search algorithm tests

packages/web/src/workers/
└── aiWorker.ts       # WebWorker for async AI processing
```

## Search Algorithm

### Iterative Deepening

The AI uses iterative deepening to gradually increase search depth:

```typescript
// Start with depth 1, increase until time limit
for (let depth = 1; depth <= maxDepth; depth++) {
    if (timeElapsed > timeLimit) break;
    
    // Search at current depth
    const result = search(position, depth);
    
    // Update best move if search completed
    if (!timeout) {
        bestMove = result.move;
        bestScore = result.score;
    }
}
```

### Alpha-Beta Pruning

Efficient pruning reduces the search space:

```typescript
function alphaBeta(position, depth, alpha, beta) {
    if (depth === 0) return evaluate(position);
    
    for (const move of generateMoves(position)) {
        const score = -alphaBeta(makeMove(position, move), depth - 1, -beta, -alpha);
        
        if (score >= beta) return beta;  // Beta cutoff
        if (score > alpha) alpha = score;
    }
    
    return alpha;
}
```

### Move Ordering

Moves are ordered to maximize pruning efficiency:

1. **Principal Variation (PV)**: Best move from previous iteration
2. **Killer Moves**: Non-capture moves that caused beta cutoffs
3. **Captures**: Ordered by MVV-LVA (Most Valuable Victim - Least Valuable Attacker)
4. **Promotions**: Moves that promote pieces
5. **Checks**: Moves that give check
6. **Central Moves**: Moves toward the center of the board

### Transposition Table

Positions are cached to avoid redundant calculations:

```typescript
class TranspositionTable {
    private table: Map<string, TranspositionEntry>;
    
    get(position: Position): TranspositionEntry | undefined {
        const key = hash(position);
        return this.table.get(key);
    }
    
    set(position: Position, entry: TranspositionEntry): void {
        const key = hash(position);
        this.table.set(key, entry);
    }
}
```

## Evaluation Function

### Components

The evaluation function considers multiple factors:

#### 1. Material Balance
- Traditional piece values (Pawn=100, Rook=1040, etc.)
- Promoted piece bonuses (Tokin=520, Dragon=1190, etc.)

#### 2. Piece-Square Tables
Each piece type has a position evaluation table:

```typescript
// Example: Pawn position values (先手視点)
const PAWN_TABLE = [
    // 9段目から1段目へ
    0,  0,  0,  0,  0,  0,  0,  0,  0,  // 9段目
    15, 15, 15, 20, 25, 20, 15, 15, 15, // 8段目
    10, 10, 10, 15, 20, 15, 10, 10, 10, // 7段目
    // ... more rows
];
```

#### 3. King Safety
- Defender pieces around king (+15 for Gold, +10 for Silver)
- Check penalties (-50 points)
- Enemy attack pressure

#### 4. Piece Mobility
- Number of legal moves for each piece
- Major pieces (Rook, Bishop) weighted 2x

#### 5. Piece Coordination
- Bonus for multiple pieces controlling same squares
- Encourages piece cooperation

### Evaluation Weights

```typescript
const total = material 
    + position 
    + kingSafety * 2.0      // King safety is critical
    + mobility * 0.1        // Small mobility bonus
    + coordination * 0.5;   // Medium coordination bonus
```

## Performance Characteristics

### Search Depth by Difficulty

| Difficulty | Depth | Time Limit | Nodes/Second |
|------------|-------|------------|--------------|
| Beginner   | 2     | 1 sec      | ~10,000      |
| Intermediate| 4     | 3 sec      | ~50,000      |
| Advanced   | 6     | 5 sec      | ~100,000     |
| Expert     | 8     | 10 sec     | ~200,000     |

### Optimizations

1. **WebWorker Integration**: Runs in background thread
2. **Incremental Updates**: Reuses calculations when possible
3. **Early Termination**: Returns best move if time runs out
4. **Lazy Evaluation**: Only evaluates positions when needed

## Testing

### Unit Tests

- **Evaluation Tests**: Verify correct position assessment
- **Search Tests**: Ensure legal moves and time limits
- **Integration Tests**: Complete game scenarios

### Performance Benchmarks

Run benchmarks with:

```bash
npm test -- --run --reporter=verbose src/services/ai/
```

## Future Improvements

### Short Term
- [ ] Opening book integration
- [ ] Endgame tablebase
- [ ] Parallel search with multiple workers
- [ ] Aspiration windows

### Long Term
- [ ] Neural network evaluation
- [ ] Monte Carlo Tree Search (MCTS)
- [ ] Self-play training
- [ ] Cloud-based analysis

## Configuration

### Tuning Parameters

Edit `AI_DIFFICULTY_CONFIGS` in `types/ai.ts`:

```typescript
export const AI_DIFFICULTY_CONFIGS = {
    beginner: {
        searchDepth: 2,
        timeLimit: 1000,
        useOpeningBook: false,
        useEndgameDatabase: false,
    },
    // ... other difficulties
};
```

### Debug Mode

Enable debug logging:

```typescript
// In engine.ts
const DEBUG = true; // Set to true for verbose logging
```

## References

- [Chess Programming Wiki](https://www.chessprogramming.org/)
- [Shogi Programming](http://www.geocities.jp/shogi_depot/index.html)
- [Alpha-Beta Pruning](https://en.wikipedia.org/wiki/Alpha%E2%80%93beta_pruning)
- [Iterative Deepening](https://www.chessprogramming.org/Iterative_Deepening)