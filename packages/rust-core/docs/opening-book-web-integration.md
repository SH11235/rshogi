# Opening Book Web Integration Plan

## Overview

This document outlines the implementation plan for integrating the 105MB YaneuraOu opening book binary data into the Shogi web application. The goal is to provide fast, efficient access to opening moves while maintaining good user experience.

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Phase 1: Data Optimization](#phase-1-data-optimization)
3. [Phase 2: WebAssembly Integration](#phase-2-webassembly-integration)
4. [Phase 3: Web Implementation](#phase-3-web-implementation)
5. [Phase 4: UI/UX Integration](#phase-4-uiux-integration)
6. [Phase 5: Performance Optimization](#phase-5-performance-optimization)
7. [Phase 6: Implementation Details](#phase-6-implementation-details)
8. [Phase 7: Deployment Strategy](#phase-7-deployment-strategy)
9. [Technical Requirements](#technical-requirements)
10. [Timeline](#timeline)

## Architecture Overview

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│   Binary Data   │────▶│  WebAssembly     │────▶│   Web App UI    │
│  (Compressed)   │     │  (Rust Core)     │     │    (React)      │
└─────────────────┘     └──────────────────┘     └─────────────────┘
        │                       │                         │
        ▼                       ▼                         ▼
   CDN/Static              Memory-Mapped            Opening Book
   Hosting                 Hash Table               Component
```

## Phase 1: Data Optimization

### 1.1 Size Reduction Strategy

Create multiple versions of the opening book for different use cases:

```bash
# Early game version (5-10MB) - First 20 moves only
./convert_opening_book \
  --input user_book1.db \
  --output opening_book_early.bin.gz \
  --max-moves 20 \
  --min-depth 5 \
  --compress

# Standard version (20-30MB) - Up to 50 moves
./convert_opening_book \
  --input user_book1.db \
  --output opening_book_standard.bin.gz \
  --max-moves 50 \
  --min-depth 5 \
  --min-eval=-500 \
  --max-eval=500 \
  --compress

# Full version (50MB+) - Complete data
./convert_opening_book \
  --input user_book1.db \
  --output opening_book_full.bin.gz \
  --compress
```

### 1.2 Progressive Loading Strategy

- Load `opening_book_early.bin` on app initialization
- Load `opening_book_standard.bin` after initial UI render
- Load `opening_book_full.bin` on demand or in background

## Phase 2: WebAssembly Integration

### 2.1 Rust API Design

Create `src/opening_book_reader.rs`:

```rust
use wasm_bindgen::prelude::*;
use std::collections::HashMap;

#[wasm_bindgen]
pub struct OpeningBookReader {
    positions: HashMap<u64, CompactPosition>,
    move_data: Vec<CompactMove>,
    index: HashMap<u64, (usize, u8)>, // position_hash -> (offset, count)
}

#[wasm_bindgen]
impl OpeningBookReader {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
            move_data: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Load compressed binary data
    pub fn load_compressed(&mut self, data: &[u8]) -> Result<JsValue, JsValue> {
        // Decompress and parse binary data
        // Build hash index for O(1) lookup
    }

    /// Find moves for a given position
    pub fn find_moves(&self, sfen: &str) -> Result<JsValue, JsValue> {
        // 1. Parse SFEN and compute hash
        // 2. Lookup in index
        // 3. Return moves as JSON
    }

    /// Get memory usage statistics
    pub fn get_stats(&self) -> JsValue {
        // Return memory usage, position count, etc.
    }
}
```

### 2.2 Move Information Structure

```rust
#[derive(Serialize)]
pub struct MoveInfo {
    pub notation: String,      // e.g., "7g7f"
    pub evaluation: i16,       // Centipawn evaluation
    pub depth: u8,            // Search depth
    pub frequency: f32,       // Usage frequency (0.0-1.0)
}
```

## Phase 3: Web Implementation

### 3.1 Data Fetching Service

Create `packages/web/src/services/openingBook/loader.ts`:

```typescript
export interface LoadProgress {
  loaded: number;
  total: number;
  phase: 'downloading' | 'decompressing' | 'indexing';
}

export class OpeningBookLoader {
  private static readonly CDN_BASE = process.env.NEXT_PUBLIC_CDN_URL || '/data';
  private static readonly CACHE_NAME = 'opening-book-v1';

  async loadBook(
    level: 'early' | 'standard' | 'full',
    onProgress?: (progress: LoadProgress) => void
  ): Promise<Uint8Array> {
    const url = `${this.CDN_BASE}/opening_book_${level}.bin.gz`;
    
    // Check cache first
    const cached = await this.checkCache(url);
    if (cached) return cached;

    // Fetch with progress
    const response = await fetch(url);
    const reader = response.body!.getReader();
    const contentLength = +response.headers.get('Content-Length')!;

    let receivedLength = 0;
    const chunks: Uint8Array[] = [];

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      chunks.push(value);
      receivedLength += value.length;

      onProgress?.({
        loaded: receivedLength,
        total: contentLength,
        phase: 'downloading'
      });
    }

    // Combine chunks
    const data = new Uint8Array(receivedLength);
    let position = 0;
    for (const chunk of chunks) {
      data.set(chunk, position);
      position += chunk.length;
    }

    // Cache for offline use
    await this.cacheData(url, data);

    return data;
  }

  private async checkCache(url: string): Promise<Uint8Array | null> {
    if (!('caches' in window)) return null;
    
    const cache = await caches.open(this.CACHE_NAME);
    const response = await cache.match(url);
    
    if (response) {
      const data = await response.arrayBuffer();
      return new Uint8Array(data);
    }
    
    return null;
  }

  private async cacheData(url: string, data: Uint8Array): Promise<void> {
    if (!('caches' in window)) return;
    
    const cache = await caches.open(this.CACHE_NAME);
    const response = new Response(data);
    await cache.put(url, response);
  }
}
```

### 3.2 Opening Book Service

Create `packages/web/src/services/openingBook/service.ts`:

```typescript
import { OpeningBookReader } from '@/wasm/opening_book';

export interface OpeningMove {
  move: string;
  evaluation: number;
  depth: number;
  frequency: number;
}

export class OpeningBookService {
  private reader?: OpeningBookReader;
  private loader: OpeningBookLoader;
  private loadedLevel: 'none' | 'early' | 'standard' | 'full' = 'none';

  constructor() {
    this.loader = new OpeningBookLoader();
  }

  async initialize(): Promise<void> {
    // Load WASM module
    const wasm = await import('@/wasm/opening_book');
    this.reader = new wasm.OpeningBookReader();
    
    // Load early game data by default
    await this.loadBook('early');
  }

  async loadBook(
    level: 'early' | 'standard' | 'full',
    onProgress?: (progress: LoadProgress) => void
  ): Promise<void> {
    if (!this.reader) {
      throw new Error('OpeningBookService not initialized');
    }

    // Don't reload if already loaded
    if (this.loadedLevel === level || 
        (this.loadedLevel === 'full') ||
        (this.loadedLevel === 'standard' && level === 'early')) {
      return;
    }

    const data = await this.loader.loadBook(level, onProgress);
    
    onProgress?.({ loaded: 0, total: 100, phase: 'decompressing' });
    await this.reader.load_compressed(data);
    
    this.loadedLevel = level;
  }

  async findMoves(sfen: string): Promise<OpeningMove[]> {
    if (!this.reader) {
      throw new Error('OpeningBookService not initialized');
    }

    try {
      const moves = await this.reader.find_moves(sfen);
      return JSON.parse(moves) as OpeningMove[];
    } catch (error) {
      console.error('Failed to find moves:', error);
      return [];
    }
  }

  getStats(): { positionCount: number; memoryUsage: number } {
    if (!this.reader) {
      return { positionCount: 0, memoryUsage: 0 };
    }

    const stats = JSON.parse(this.reader.get_stats());
    return stats;
  }
}

// Singleton instance
export const openingBookService = new OpeningBookService();
```

## Phase 4: UI/UX Integration

### 4.1 Opening Book Component

Create `packages/web/src/components/OpeningBook.tsx`:

```tsx
import React, { useEffect, useState } from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { openingBookService, OpeningMove } from '@/services/openingBook/service';
import { useGameStore } from '@/stores/gameStore';

export const OpeningBook: React.FC = () => {
  const { getCurrentSfen, makeMove } = useGameStore();
  const [moves, setMoves] = useState<OpeningMove[]>([]);
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState(0);
  const [dataLevel, setDataLevel] = useState<'early' | 'standard' | 'full'>('early');

  useEffect(() => {
    loadMoves();
  }, [getCurrentSfen()]);

  const loadMoves = async () => {
    const sfen = getCurrentSfen();
    const foundMoves = await openingBookService.findMoves(sfen);
    setMoves(foundMoves);
  };

  const loadMoreData = async (level: 'standard' | 'full') => {
    setLoading(true);
    setProgress(0);

    try {
      await openingBookService.loadBook(level, (prog) => {
        setProgress((prog.loaded / prog.total) * 100);
      });
      setDataLevel(level);
      await loadMoves();
    } finally {
      setLoading(false);
    }
  };

  const formatEval = (eval: number): string => {
    if (eval > 0) return `+${eval}`;
    return eval.toString();
  };

  return (
    <Card className="w-full max-w-md">
      <CardHeader>
        <CardTitle>定跡</CardTitle>
      </CardHeader>
      <CardContent>
        {loading && (
          <div className="mb-4">
            <Progress value={progress} className="mb-2" />
            <p className="text-sm text-muted-foreground">
              データ読み込み中... {Math.round(progress)}%
            </p>
          </div>
        )}

        {moves.length === 0 ? (
          <p className="text-muted-foreground">
            この局面の定跡データはありません
          </p>
        ) : (
          <div className="space-y-2">
            {moves.map((move, index) => (
              <Button
                key={index}
                variant="outline"
                className="w-full justify-between"
                onClick={() => makeMove(move.move)}
              >
                <span>{move.move}</span>
                <span className="text-sm">
                  評価: {formatEval(move.evaluation)} 
                  深さ: {move.depth}
                  頻度: {(move.frequency * 100).toFixed(1)}%
                </span>
              </Button>
            ))}
          </div>
        )}

        <div className="mt-4 space-x-2">
          {dataLevel === 'early' && (
            <Button
              size="sm"
              variant="secondary"
              onClick={() => loadMoreData('standard')}
              disabled={loading}
            >
              より多くの定跡を読み込む
            </Button>
          )}
          {dataLevel === 'standard' && (
            <Button
              size="sm"
              variant="secondary"
              onClick={() => loadMoreData('full')}
              disabled={loading}
            >
              全ての定跡を読み込む
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
};
```

### 4.2 Game Store Integration

Update `packages/web/src/stores/gameStore.ts`:

```typescript
interface GameState {
  // ... existing state ...
  openingBookEnabled: boolean;
  suggestedMove: string | null;
}

export const useGameStore = create<GameState & GameActions>((set, get) => ({
  // ... existing implementation ...
  
  openingBookEnabled: true,
  suggestedMove: null,

  checkOpeningBook: async () => {
    if (!get().openingBookEnabled) return;

    const sfen = get().getCurrentSfen();
    const moves = await openingBookService.findMoves(sfen);
    
    if (moves.length > 0) {
      // Set the best move as suggestion
      set({ suggestedMove: moves[0].move });
    } else {
      set({ suggestedMove: null });
    }
  },

  toggleOpeningBook: () => {
    set((state) => ({ 
      openingBookEnabled: !state.openingBookEnabled,
      suggestedMove: null 
    }));
  },
}));
```

## Phase 5: Performance Optimization

### 5.1 Web Worker Implementation

Create `packages/web/src/workers/openingBook.worker.ts`:

```typescript
import { OpeningBookReader } from '@/wasm/opening_book';

let reader: OpeningBookReader | null = null;

self.addEventListener('message', async (event) => {
  const { type, data } = event.data;

  switch (type) {
    case 'init':
      reader = new OpeningBookReader();
      self.postMessage({ type: 'ready' });
      break;

    case 'load':
      if (!reader) return;
      try {
        await reader.load_compressed(data.buffer);
        self.postMessage({ type: 'loaded' });
      } catch (error) {
        self.postMessage({ type: 'error', error });
      }
      break;

    case 'find':
      if (!reader) return;
      try {
        const moves = reader.find_moves(data.sfen);
        self.postMessage({ type: 'moves', moves: JSON.parse(moves) });
      } catch (error) {
        self.postMessage({ type: 'error', error });
      }
      break;
  }
});
```

### 5.2 Memory Management

```typescript
export class MemoryEfficientOpeningBook {
  private cache: LRUCache<string, OpeningMove[]>;
  private hotPositions: Set<string>;

  constructor(maxCacheSize: number = 1000) {
    this.cache = new LRUCache({ max: maxCacheSize });
    this.hotPositions = new Set();
  }

  async preloadCommonPositions(): Promise<void> {
    // Preload first 10 moves of common openings
    const commonOpenings = [
      "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1", // Initial
      // ... more common positions
    ];

    for (const sfen of commonOpenings) {
      const moves = await openingBookService.findMoves(sfen);
      this.cache.set(sfen, moves);
      this.hotPositions.add(sfen);
    }
  }
}
```

## Phase 6: Implementation Details

### 6.1 Binary Format Specification

The binary format consists of:

1. **Header** (16 bytes)
   - Magic: "SFEN" (4 bytes)
   - Version: 1 (4 bytes)
   - Position count (4 bytes)
   - Checksum (4 bytes)

2. **Position Entries** (16 bytes each)
   - Position hash (8 bytes)
   - Best move (2 bytes)
   - Evaluation (2 bytes)
   - Depth (1 byte)
   - Move count (1 byte)
   - Popularity (1 byte)
   - Reserved (1 byte)

3. **Move Entries** (6 bytes each)
   - Move encoded (2 bytes)
   - Evaluation (2 bytes)
   - Depth (1 byte)
   - Reserved (1 byte)

### 6.2 Hash Calculation

Position hashes are calculated using Zobrist hashing:

```rust
pub fn calculate_position_hash(sfen: &str) -> u64 {
    let position = parse_sfen(sfen);
    let mut hash = 0u64;
    
    // Hash board pieces
    for (square, piece) in position.board {
        hash ^= ZOBRIST_PIECES[square][piece];
    }
    
    // Hash turn
    if position.turn == 'w' {
        hash ^= ZOBRIST_TURN;
    }
    
    // Hash hand pieces
    for (piece_type, count) in position.hand {
        hash ^= ZOBRIST_HAND[piece_type] * count as u64;
    }
    
    hash
}
```

## Phase 7: Deployment Strategy

### 7.1 CDN Configuration

```nginx
# Nginx configuration for optimal delivery
location /data/ {
    # Enable gzip compression
    gzip on;
    gzip_types application/octet-stream;
    
    # Cache headers
    add_header Cache-Control "public, max-age=31536000, immutable";
    
    # CORS for web workers
    add_header Access-Control-Allow-Origin *;
    
    # Brotli compression if available
    brotli on;
    brotli_types application/octet-stream;
}
```

### 7.2 Version Management

```typescript
export const OPENING_BOOK_VERSION = '1.0.0';
export const OPENING_BOOK_URLS = {
  early: `/data/opening_book_early_${OPENING_BOOK_VERSION}.bin.gz`,
  standard: `/data/opening_book_standard_${OPENING_BOOK_VERSION}.bin.gz`,
  full: `/data/opening_book_full_${OPENING_BOOK_VERSION}.bin.gz`,
};
```

## Technical Requirements

### Performance Targets

- **Initial Load**: < 3 seconds for early game data
- **Memory Usage**: < 200MB maximum
- **Lookup Time**: < 1ms per position
- **Compression Ratio**: 70-90% reduction

### Browser Compatibility

- Chrome 80+
- Firefox 75+
- Safari 14+
- Edge 80+

### Required Features

- WebAssembly
- Web Workers
- Cache API
- IndexedDB (for persistent storage)

## Timeline

### Week 1: Foundation
- [ ] Implement Rust WebAssembly API
- [ ] Create binary reader functionality
- [ ] Set up build pipeline

### Week 2: Web Integration
- [ ] Implement TypeScript service layer
- [ ] Create data loader with progress
- [ ] Add caching mechanism

### Week 3: UI Implementation
- [ ] Create OpeningBook component
- [ ] Integrate with game store
- [ ] Add loading indicators

### Week 4: Optimization
- [ ] Implement Web Worker
- [ ] Add memory management
- [ ] Performance testing
- [ ] Deploy to CDN

## Testing Strategy

### Unit Tests
- Position hash calculation
- Move encoding/decoding
- Binary format parsing

### Integration Tests
- Data loading pipeline
- Cache functionality
- UI interactions

### Performance Tests
- Load time benchmarks
- Memory usage monitoring
- Lookup speed testing

## Monitoring

### Key Metrics
- Data download time
- Cache hit rate
- Memory usage
- Lookup performance

### Error Tracking
- Failed downloads
- Parsing errors
- Out of memory errors

## Future Enhancements

1. **Incremental Updates**: Support for updating opening book without full redownload
2. **User Contributions**: Allow users to contribute their own opening lines
3. **Opening Training Mode**: Interactive learning of opening theory
4. **Cloud Sync**: Synchronize learned openings across devices
5. **AI Integration**: Use opening book to improve AI play in early game