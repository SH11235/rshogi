# Opening Book Implementation Quick Guide

## Quick Start

This guide provides step-by-step instructions for implementing the opening book feature in the Shogi web application. The implementation starts with the early game version (13MB) which provides excellent coverage of common openings while maintaining fast load times.

## Step 1: Generate Optimized Binary Files (または既存ファイルを使用)

**注意**: 既に `converted_openings/` ディレクトリに生成済みのファイルがある場合は、このステップをスキップして Step 2 に進んでください。

### About the Source Data

The `user_book1.db` file is from the YaneuraOu project:
- **Source**: https://github.com/yaneurao/YaneuraOu
- **License**: MIT License
- **Copyright**: (c) YaneuraOu project contributors
- **Size**: ~470MB (original file)

This file contains millions of analyzed shogi positions and is freely available for use under the MIT License.

生成済みファイル:
- `opening_book_early.bin.gz` (13MB) - 初期実装に推奨
- `opening_book_web.bin.gz` (24MB) - 標準的なWeb配信用
- `opening_book_tournament.bin.gz` (8.6MB) - 厳選された高品質定跡
- `opening_book_full.bin.gz` (67MB) - 完全なデータセット

新規に生成する場合:

```bash
cd packages/rust-core

# Generate early game version (recommended for initial implementation)
# This creates a 13MB file perfect for initial web deployment
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_early.bin.gz \
  --max-moves 20 \
  --min-depth 5 \
  --min-eval=-500 \
  --max-eval=500 \
  --compress \
  --validate

# OPTIONAL: Generate other versions for future use
# Standard version (24MB) - more comprehensive coverage
# ./target/release/convert_opening_book \
#   --input user_book1.db \
#   --output converted_openings/opening_book_web.bin.gz \
#   --max-moves 50 \
#   --min-depth 5 \
#   --min-eval=-500 \
#   --max-eval=500 \
#   --compress \
#   --validate
```

## Step 2: Implement Rust WebAssembly Reader

### 2.1 Create `src/opening_book_reader.rs`

```rust
use wasm_bindgen::prelude::*;
use crate::opening_book::{BinaryConverter, PositionHasher, MoveEncoder};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct BookMove {
    pub notation: String,
    pub evaluation: i16,
    pub depth: u8,
}

#[wasm_bindgen]
pub struct OpeningBookReader {
    converter: BinaryConverter,
    positions: HashMap<u64, Vec<BookMove>>,
}

#[wasm_bindgen]
impl OpeningBookReader {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            converter: BinaryConverter::new(),
            positions: HashMap::new(),
        }
    }

    pub fn load_data(&mut self, compressed_data: &[u8]) -> Result<String, JsValue> {
        // Decompress data
        let data = self.converter
            .decompress_data(compressed_data)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        // Parse binary format
        let mut cursor = std::io::Cursor::new(data);
        let entries = self.converter
            .read_binary(&mut cursor)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        
        // Build position map
        for entry in entries {
            let moves: Vec<BookMove> = entry.moves.iter().map(|m| {
                let notation = MoveEncoder::decode_move(m.move_encoded)
                    .unwrap_or_else(|_| "error".to_string());
                BookMove {
                    notation,
                    evaluation: m.evaluation,
                    depth: m.depth,
                }
            }).collect();
            
            self.positions.insert(entry.header.position_hash, moves);
        }
        
        Ok(format!("Loaded {} positions", self.positions.len()))
    }

    pub fn find_moves(&self, sfen: &str) -> String {
        let hash = match PositionHasher::hash_position(sfen) {
            Ok(h) => h,
            Err(_) => return "[]".to_string(),
        };
        
        match self.positions.get(&hash) {
            Some(moves) => serde_json::to_string(moves).unwrap_or_else(|_| "[]".to_string()),
            None => "[]".to_string(),
        }
    }
}
```

### 2.2 Update `src/lib.rs`

```rust
pub mod opening_book;
pub mod opening_book_reader;

pub use opening_book_reader::*;
```

### 2.3 Build WebAssembly

```bash
# From packages/rust-core
npm run build:wasm
```

## Step 3: TypeScript Integration

### 3.1 Create Type Definitions

Create `packages/web/src/types/openingBook.ts`:

```typescript
export interface BookMove {
  notation: string;
  evaluation: number;
  depth: number;
}

export interface LoadProgress {
  phase: 'downloading' | 'decompressing' | 'indexing';
  loaded: number;
  total: number;
}

export type BookLevel = 'early' | 'standard' | 'full';
```

### 3.2 Create Opening Book Service

Create `packages/web/src/services/openingBook.ts`:

```typescript
import type { BookMove, LoadProgress, BookLevel } from '@/types/openingBook';

class OpeningBookService {
  private reader?: any; // Will be OpeningBookReader from WASM
  private initialized = false;
  private currentLevel: BookLevel | null = null;

  async initialize(): Promise<void> {
    if (this.initialized) return;

    // Dynamic import to avoid SSR issues
    const wasm = await import('@/wasm/shogi_core');
    await wasm.default();
    
    this.reader = new wasm.OpeningBookReader();
    this.initialized = true;
  }

  async loadBook(
    level: BookLevel = 'early',
    onProgress?: (progress: LoadProgress) => void
  ): Promise<void> {
    if (!this.initialized) {
      await this.initialize();
    }

    // Skip if already loaded
    if (this.currentLevel === level) return;

    const url = `/data/opening_book_${level}.bin.gz`;
    
    // Download with progress
    const response = await fetch(url);
    const contentLength = Number(response.headers.get('content-length'));
    const reader = response.body!.getReader();
    
    let receivedLength = 0;
    const chunks: Uint8Array[] = [];

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      chunks.push(value);
      receivedLength += value.length;

      onProgress?.({
        phase: 'downloading',
        loaded: receivedLength,
        total: contentLength,
      });
    }

    // Combine chunks
    const data = new Uint8Array(receivedLength);
    let position = 0;
    for (const chunk of chunks) {
      data.set(chunk, position);
      position += chunk.length;
    }

    // Load into WASM
    onProgress?.({ phase: 'decompressing', loaded: 0, total: 100 });
    await this.reader!.load_data(data);
    
    this.currentLevel = level;
  }

  async findMoves(sfen: string): Promise<BookMove[]> {
    if (!this.initialized || !this.reader) return [];

    try {
      const movesJson = this.reader.find_moves(sfen);
      return JSON.parse(movesJson);
    } catch (error) {
      console.error('Failed to find moves:', error);
      return [];
    }
  }
}

export const openingBook = new OpeningBookService();
```

## Step 4: React Hook Implementation

Create `packages/web/src/hooks/useOpeningBook.ts`:

```typescript
import { useState, useEffect, useCallback } from 'react';
import { openingBook } from '@/services/openingBook';
import type { BookMove, LoadProgress, BookLevel } from '@/types/openingBook';

export function useOpeningBook(sfen: string) {
  const [moves, setMoves] = useState<BookMove[]>([]);
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState<LoadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [level, setLevel] = useState<BookLevel>('early');

  // Initialize and load early data on mount
  useEffect(() => {
    openingBook.initialize()
      .then(() => openingBook.loadBook('early'))
      .catch(err => setError(err.message));
  }, []);

  // Find moves when position changes
  useEffect(() => {
    if (!sfen) return;

    openingBook.findMoves(sfen)
      .then(setMoves)
      .catch(err => {
        console.error('Failed to find moves:', err);
        setMoves([]);
      });
  }, [sfen]);

  const loadMoreData = useCallback(async (newLevel: BookLevel) => {
    if (loading || level === newLevel) return;

    setLoading(true);
    setError(null);

    try {
      await openingBook.loadBook(newLevel, setProgress);
      setLevel(newLevel);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load data');
    } finally {
      setLoading(false);
      setProgress(null);
    }
  }, [loading, level]);

  return {
    moves,
    loading,
    progress,
    error,
    level,
    loadMoreData,
  };
}
```

## Step 5: UI Component

Create `packages/web/src/components/OpeningBook/OpeningBook.tsx`:

```tsx
import React from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { useOpeningBook } from '@/hooks/useOpeningBook';
import { useGameStore } from '@/stores/gameStore';
import { BookMove } from '@/types/openingBook';

export function OpeningBook() {
  const sfen = useGameStore(state => state.getCurrentSfen());
  const makeMove = useGameStore(state => state.makeMove);
  
  const { moves, loading, progress, error, level, loadMoreData } = useOpeningBook(sfen);

  const handleMoveClick = (move: BookMove) => {
    makeMove(move.notation);
  };

  const formatEval = (eval: number): string => {
    const sign = eval > 0 ? '+' : '';
    return `${sign}${eval}`;
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>定跡</CardTitle>
      </CardHeader>
      <CardContent>
        {error && (
          <div className="text-red-500 text-sm mb-4">{error}</div>
        )}

        {loading && progress && (
          <div className="mb-4">
            <Progress 
              value={progress.phase === 'downloading' 
                ? (progress.loaded / progress.total) * 100 
                : 50
              } 
            />
            <p className="text-sm text-muted-foreground mt-1">
              {progress.phase === 'downloading' ? '読み込み中' : '解凍中'}...
            </p>
          </div>
        )}

        {moves.length === 0 ? (
          <p className="text-muted-foreground">
            この局面の定跡はありません
          </p>
        ) : (
          <div className="space-y-2">
            {moves.map((move, idx) => (
              <Button
                key={idx}
                variant="outline"
                className="w-full justify-between"
                onClick={() => handleMoveClick(move)}
              >
                <span className="font-mono">{move.notation}</span>
                <span className="text-sm text-muted-foreground">
                  評価: {formatEval(move.evaluation)} 深さ: {move.depth}
                </span>
              </Button>
            ))}
          </div>
        )}

        <div className="mt-4 flex gap-2">
          {level === 'early' && (
            <Button
              size="sm"
              variant="secondary"
              onClick={() => loadMoreData('standard')}
              disabled={loading}
            >
              標準定跡を読み込む
            </Button>
          )}
          {level === 'standard' && (
            <Button
              size="sm"
              variant="secondary"
              onClick={() => loadMoreData('full')}
              disabled={loading}
            >
              全定跡を読み込む
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
```

## Step 6: Integration with Game

### 6.1 Add to Game Layout

Update `packages/web/src/app/game/page.tsx`:

```tsx
import { OpeningBook } from '@/components/OpeningBook/OpeningBook';

export default function GamePage() {
  return (
    <div className="container mx-auto p-4">
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        <div className="lg:col-span-2">
          <Board />
        </div>
        <div className="space-y-4">
          <GameInfo />
          <OpeningBook />
          <CapturedPieces />
        </div>
      </div>
    </div>
  );
}
```

### 6.2 Update Game Store

Add to `packages/web/src/stores/gameStore.ts`:

```typescript
interface GameState {
  // ... existing state
  getCurrentSfen: () => string;
}

// In the store implementation:
getCurrentSfen: () => {
  const state = get();
  // Convert current position to SFEN format
  // Implementation depends on your game state structure
  return convertToSfen(state.board, state.currentPlayer, state.capturedPieces);
},
```

## Step 7: Deploy Data Files

### 7.1 Copy to Public Directory

```bash
# Create data directory if it doesn't exist
mkdir -p packages/web/public/data/

# Copy early game version first (13MB - recommended for initial deployment)
cp packages/rust-core/converted_openings/opening_book_early.bin.gz packages/web/public/data/

# OPTIONAL: Copy other versions for progressive loading
# Standard version (24MB)
# cp packages/rust-core/converted_openings/opening_book_web.bin.gz packages/web/public/data/
# Full version (67MB) - only if needed
# cp packages/rust-core/converted_openings/opening_book_full.bin.gz packages/web/public/data/
```

### 7.2 Configure Next.js

Update `packages/web/next.config.js`:

```javascript
/** @type {import('next').NextConfig} */
const nextConfig = {
  // ... existing config
  
  // Optimize large static files
  compress: true,
  
  // Headers for caching
  async headers() {
    return [
      {
        source: '/data/:path*',
        headers: [
          {
            key: 'Cache-Control',
            value: 'public, max-age=31536000, immutable',
          },
        ],
      },
    ];
  },
};
```

## Verification of Generated Files

### Verify Binary Files Before Deployment

Use the `verify_opening_book` tool to ensure the generated files are correct:

```bash
# Verify early game version
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_early.bin.gz \
  --stats-only

# Expected output:
# Successfully loaded 436484 positions
# Total moves: 2328762
# Average moves per position: 5.34
```

### Check Initial Position

```bash
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_early.bin.gz \
  --check-position "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"

# Should show:
# Position FOUND in binary!
# Best move: 7g7f (eval: 44, depth: 30)
# With 8 available moves including 2g2f, 7g7f, 6i7h, etc.
```

## Testing

### Manual Testing Steps

1. **Load Test**
   ```
   - Open browser dev tools
   - Navigate to game page
   - Check Network tab for opening_book_early.bin.gz
   - Verify file loads successfully
   ```

2. **Functionality Test**
   ```
   - Make a few standard opening moves
   - Verify opening book shows suggestions
   - Click on suggested moves
   - Verify moves are played correctly
   ```

3. **Progressive Loading Test**
   ```
   - Click "Load Standard Opening Book"
   - Verify progress bar shows
   - Verify more moves appear after loading
   ```

### Automated Tests

Create `packages/web/src/services/__tests__/openingBook.test.ts`:

```typescript
import { openingBook } from '../openingBook';

describe('OpeningBookService', () => {
  it('should initialize successfully', async () => {
    await openingBook.initialize();
    expect(openingBook).toBeDefined();
  });

  it('should find moves for initial position', async () => {
    await openingBook.initialize();
    await openingBook.loadBook('early');
    
    const moves = await openingBook.findMoves(
      'lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1'
    );
    
    expect(moves.length).toBeGreaterThan(0);
    expect(moves[0]).toHaveProperty('notation');
    expect(moves[0]).toHaveProperty('evaluation');
    expect(moves[0]).toHaveProperty('depth');
  });
});
```

## Troubleshooting

### Common Issues

1. **WASM not loading**
   - Check that wasm files are in `packages/web/src/wasm/`
   - Verify Next.js webpack config includes wasm loader

2. **Binary files not found**
   - Ensure files are in `public/data/`
   - Check file permissions
   - Verify CDN/hosting configuration

3. **Memory issues**
   - Monitor browser memory usage
   - Consider implementing cleanup on page navigation
   - Use Web Workers for large data processing

4. **Slow loading**
   - Check compression is working
   - Verify CDN caching headers
   - Consider smaller initial data set

## Next Steps

1. **Optimization**
   - Implement Web Worker for background processing
   - Add IndexedDB caching
   - Optimize WASM bundle size

2. **Features**
   - Add opening name display
   - Show variation trees
   - Implement opening training mode

3. **Analytics**
   - Track which openings are most viewed
   - Monitor load times
   - Collect user feedback
