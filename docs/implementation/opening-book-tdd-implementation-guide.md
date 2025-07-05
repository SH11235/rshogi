# Opening Book TDD Implementation Guide

このガイドでは、定跡機能をt-wada氏のTDDスタイルで実装する手順を説明します。各ステップは必ずテストから始め、Red-Green-Refactorサイクルで進めます。

## 前提条件

- 生成済みの定跡バイナリファイルが `converted_openings/` に存在すること
- TDDの基本原則を理解していること（[TDD Implementation Guide](../development/tdd-implementation-guide.md)参照）

## 実装のTODOリスト

```
=== Phase 1: Rust WebAssembly基盤 ===
□ 1-1. OpeningBookReaderの型定義とテスト
□ 1-2. 空のコンストラクタ実装
□ 1-3. バイナリデータ読み込みインターフェース
□ 1-4. 圧縮データ解凍機能
□ 1-5. 位置データのパース
□ 1-6. ハッシュ値による検索機能
□ 1-7. SFEN文字列からの検索
□ 1-8. WebAssemblyエクスポート

=== Phase 2: TypeScriptサービス層 ===
□ 2-1. OpeningBookService型定義
□ 2-2. WASM初期化テスト
□ 2-3. データダウンロード機能
□ 2-4. 進捗通知機能
□ 2-5. エラーハンドリング
□ 2-6. キャッシュ機能

=== Phase 3: React統合 ===
□ 3-1. useOpeningBookフック基本構造
□ 3-2. 初期化と自動読み込み
□ 3-3. 局面変更時の自動検索
□ 3-4. エラー状態の管理
□ 3-5. 進捗状態の管理

=== Phase 4: UIコンポーネント ===
□ 4-1. OpeningBookコンポーネント基本表示
□ 4-2. 定跡手のリスト表示
□ 4-3. クリックイベント処理
□ 4-4. 読み込み状態表示
□ 4-5. エラー表示
□ 4-6. データレベル切り替え
```

## Phase 1: Rust WebAssembly基盤

### Step 1-1: OpeningBookReaderの型定義とテスト

**Red: 最初のテストを書く**

`src/opening_book_reader.rs`のテストモジュールを作成：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_new_reader() {
        // Arrange & Act
        let reader = OpeningBookReader::new();
        
        // Assert
        assert_eq!(reader.position_count(), 0);
        assert!(!reader.is_loaded());
    }
}
```

**Green: 最小限の実装**

```rust
use std::collections::HashMap;

pub struct OpeningBookReader {
    positions: HashMap<u64, Vec<BookMove>>,
    loaded: bool,
}

#[derive(Debug, Clone)]
pub struct BookMove {
    pub notation: String,
    pub evaluation: i16,
    pub depth: u8,
}

impl OpeningBookReader {
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
            loaded: false,
        }
    }
    
    pub fn position_count(&self) -> usize {
        self.positions.len()
    }
    
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }
}
```

**Refactor: 必要に応じて整理（この段階では不要）**

### Step 1-2: バイナリデータ読み込みインターフェースのテスト

**Red: データ読み込みのテスト**

```rust
#[test]
fn test_load_empty_data() {
    // Arrange
    let mut reader = OpeningBookReader::new();
    let empty_data = vec![];
    
    // Act
    let result = reader.load_data(&empty_data);
    
    // Assert
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Loaded 0 positions");
    assert!(reader.is_loaded());
}

#[test]
fn test_load_invalid_data() {
    // Arrange
    let mut reader = OpeningBookReader::new();
    let invalid_data = vec![1, 2, 3]; // 無効なデータ
    
    // Act
    let result = reader.load_data(&invalid_data);
    
    // Assert
    assert!(result.is_err());
    assert!(!reader.is_loaded());
}
```

**Green: load_dataメソッドの実装**

```rust
use std::io::{self, Cursor};

impl OpeningBookReader {
    pub fn load_data(&mut self, compressed_data: &[u8]) -> Result<String, String> {
        if compressed_data.is_empty() {
            self.loaded = true;
            return Ok("Loaded 0 positions".to_string());
        }
        
        // とりあえず無効なデータはエラーにする
        if compressed_data.len() < 4 {
            return Err("Invalid data format".to_string());
        }
        
        self.loaded = true;
        Ok("Loaded 0 positions".to_string())
    }
}
```

### Step 1-3: 圧縮データ解凍機能のテスト

**Red: 圧縮データのテスト**

```rust
#[test]
fn test_decompress_gzip_data() {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    
    // Arrange
    let original_data = b"test data";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(original_data).unwrap();
    let compressed = encoder.finish().unwrap();
    
    let mut reader = OpeningBookReader::new();
    
    // Act
    let result = reader.decompress_data(&compressed);
    
    // Assert
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), original_data);
}
```

**Green: 解凍機能の実装**

```rust
use flate2::read::GzDecoder;
use std::io::Read;

impl OpeningBookReader {
    fn decompress_data(&self, compressed: &[u8]) -> Result<Vec<u8>, io::Error> {
        let mut decoder = GzDecoder::new(compressed);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        Ok(decompressed)
    }
}
```

### Step 1-4: 実際のバイナリフォーマット読み込みテスト

**Red: バイナリフォーマットのテスト**

```rust
#[test]
fn test_read_single_position() {
    // Arrange: 1つの位置データを作成
    let mut data = Vec::new();
    
    // ヘッダー: position_hash(8) + move_count(2) + padding(6)
    data.extend_from_slice(&12345u64.to_le_bytes()); // position_hash
    data.extend_from_slice(&1u16.to_le_bytes()); // move_count
    data.extend_from_slice(&[0u8; 6]); // padding
    
    // ムーブデータ: move_encoded(2) + evaluation(2) + depth(1) + padding(1)
    data.extend_from_slice(&0x1234u16.to_le_bytes()); // move
    data.extend_from_slice(&50i16.to_le_bytes()); // eval
    data.push(10); // depth
    data.push(0); // padding
    
    let mut reader = OpeningBookReader::new();
    
    // Act
    let result = reader.parse_binary_data(&data);
    
    // Assert
    assert!(result.is_ok());
    assert_eq!(reader.position_count(), 1);
    
    let moves = reader.find_moves_by_hash(12345);
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].evaluation, 50);
    assert_eq!(moves[0].depth, 10);
}
```

**Green: バイナリパース実装**

```rust
impl OpeningBookReader {
    fn parse_binary_data(&mut self, data: &[u8]) -> Result<(), String> {
        let mut cursor = Cursor::new(data);
        
        while cursor.position() < data.len() as u64 {
            // ヘッダー読み込み
            let mut header_buf = [0u8; 16];
            if cursor.read_exact(&mut header_buf).is_err() {
                break;
            }
            
            let position_hash = u64::from_le_bytes(header_buf[0..8].try_into().unwrap());
            let move_count = u16::from_le_bytes(header_buf[8..10].try_into().unwrap());
            
            // ムーブ読み込み
            let mut moves = Vec::new();
            for _ in 0..move_count {
                let mut move_buf = [0u8; 6];
                cursor.read_exact(&mut move_buf)
                    .map_err(|e| format!("Failed to read move: {}", e))?;
                
                let move_encoded = u16::from_le_bytes(move_buf[0..2].try_into().unwrap());
                let evaluation = i16::from_le_bytes(move_buf[2..4].try_into().unwrap());
                let depth = move_buf[4];
                
                moves.push(BookMove {
                    notation: format!("move_{}", move_encoded), // 後で実装
                    evaluation,
                    depth,
                });
            }
            
            self.positions.insert(position_hash, moves);
        }
        
        Ok(())
    }
    
    pub fn find_moves_by_hash(&self, hash: u64) -> Vec<BookMove> {
        self.positions.get(&hash)
            .map(|moves| moves.clone())
            .unwrap_or_default()
    }
}
```

### Step 1-5: SFEN文字列からの検索テスト

**Red: SFEN検索のテスト**

```rust
#[test]
fn test_find_moves_by_sfen() {
    // Arrange
    let mut reader = OpeningBookReader::new();
    let initial_sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    
    // 仮のハッシュ値とムーブを設定
    reader.positions.insert(
        123456789, // 初期局面の仮ハッシュ
        vec![BookMove {
            notation: "7g7f".to_string(),
            evaluation: 50,
            depth: 10,
        }]
    );
    
    // Act
    let moves = reader.find_moves(initial_sfen);
    
    // Assert
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].notation, "7g7f");
}
```

**Green: SFEN検索の仮実装**

```rust
impl OpeningBookReader {
    pub fn find_moves(&self, sfen: &str) -> Vec<BookMove> {
        // 仮実装：初期局面なら固定ハッシュ値を返す
        if sfen == "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1" {
            return self.find_moves_by_hash(123456789);
        }
        vec![]
    }
}
```

**実際のハッシュ計算は既存のPositionHasherを使用するため、後で統合**

### Step 1-6: WebAssemblyバインディングのテスト

**Red: WASMエクスポートのテスト**

```rust
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test]
fn test_wasm_binding() {
    // Arrange
    let reader = OpeningBookReader::new();
    
    // Act & Assert
    assert_eq!(reader.position_count(), 0);
}
```

**Green: WASMバインディング実装**

```rust
use wasm_bindgen::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct JsBookMove {
    pub notation: String,
    pub evaluation: i16,
    pub depth: u8,
}

#[wasm_bindgen]
impl OpeningBookReader {
    #[wasm_bindgen(constructor)]
    pub fn new_wasm() -> Self {
        Self::new()
    }
    
    #[wasm_bindgen]
    pub fn load_data_wasm(&mut self, compressed_data: &[u8]) -> Result<String, JsValue> {
        self.load_data(compressed_data)
            .map_err(|e| JsValue::from_str(&e))
    }
    
    #[wasm_bindgen]
    pub fn find_moves_wasm(&self, sfen: &str) -> String {
        let moves: Vec<JsBookMove> = self.find_moves(sfen)
            .into_iter()
            .map(|m| JsBookMove {
                notation: m.notation,
                evaluation: m.evaluation,
                depth: m.depth,
            })
            .collect();
        
        serde_json::to_string(&moves).unwrap_or_else(|_| "[]".to_string())
    }
}
```

## Phase 2: TypeScriptサービス層

### Step 2-1: OpeningBookService型定義とテスト

**Red: サービスの基本テスト**

`packages/web/src/services/__tests__/openingBook.test.ts`:

```typescript
import { describe, it, expect } from 'vitest';
import { OpeningBookService } from '../openingBook';

describe('OpeningBookService', () => {
  it('should create instance', () => {
    // Arrange & Act
    const service = new OpeningBookService();
    
    // Assert
    expect(service).toBeDefined();
    expect(service.isInitialized()).toBe(false);
  });
});
```

**Green: 最小限のサービス実装**

`packages/web/src/services/openingBook.ts`:

```typescript
export class OpeningBookService {
  private initialized = false;
  
  isInitialized(): boolean {
    return this.initialized;
  }
}
```

### Step 2-2: WASM初期化テスト

**Red: 初期化テスト**

```typescript
it('should initialize WASM module', async () => {
  // Arrange
  const service = new OpeningBookService();
  
  // Act
  await service.initialize();
  
  // Assert
  expect(service.isInitialized()).toBe(true);
});
```

**Green: 初期化の仮実装**

```typescript
export class OpeningBookService {
  private initialized = false;
  private reader: any = null;
  
  async initialize(): Promise<void> {
    if (this.initialized) return;
    
    // 仮実装：後でWASMを読み込む
    this.reader = {}; // モックオブジェクト
    this.initialized = true;
  }
  
  isInitialized(): boolean {
    return this.initialized;
  }
}
```

### Step 2-3: データダウンロードテスト

**Red: ダウンロード機能のテスト**

```typescript
it('should download book data', async () => {
  // Arrange
  const service = new OpeningBookService();
  await service.initialize();
  
  let progressCalled = false;
  const onProgress = () => { progressCalled = true; };
  
  // fetchをモック
  global.fetch = vi.fn().mockResolvedValue({
    headers: new Headers({ 'content-length': '100' }),
    body: {
      getReader: () => ({
        read: vi.fn()
          .mockResolvedValueOnce({ value: new Uint8Array(50), done: false })
          .mockResolvedValueOnce({ done: true })
      })
    }
  });
  
  // Act
  await service.loadBook('early', onProgress);
  
  // Assert
  expect(fetch).toHaveBeenCalledWith('/data/opening_book_early.bin.gz');
  expect(progressCalled).toBe(true);
});
```

**Green: ダウンロード実装**

```typescript
export type BookLevel = 'early' | 'standard' | 'full';
export type ProgressCallback = (progress: LoadProgress) => void;

export interface LoadProgress {
  phase: 'downloading' | 'decompressing' | 'indexing';
  loaded: number;
  total: number;
}

export class OpeningBookService {
  private initialized = false;
  private reader: any = null;
  private currentLevel: BookLevel | null = null;
  
  async loadBook(
    level: BookLevel = 'early',
    onProgress?: ProgressCallback
  ): Promise<void> {
    if (!this.initialized) {
      await this.initialize();
    }
    
    if (this.currentLevel === level) return;
    
    const url = `/data/opening_book_${level}.bin.gz`;
    const response = await fetch(url);
    const contentLength = Number(response.headers.get('content-length') || 0);
    
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
    
    // データを結合
    const data = new Uint8Array(receivedLength);
    let position = 0;
    for (const chunk of chunks) {
      data.set(chunk, position);
      position += chunk.length;
    }
    
    // WASMに読み込む（仮実装）
    onProgress?.({ phase: 'decompressing', loaded: 0, total: 100 });
    
    this.currentLevel = level;
  }
}
```

## Phase 3: React統合

### Step 3-1: useOpeningBookフックの基本テスト

**Red: フックの基本テスト**

`packages/web/src/hooks/__tests__/useOpeningBook.test.tsx`:

```typescript
import { renderHook } from '@testing-library/react';
import { describe, it, expect } from 'vitest';
import { useOpeningBook } from '../useOpeningBook';

describe('useOpeningBook', () => {
  it('should initialize with default state', () => {
    // Arrange & Act
    const { result } = renderHook(() => 
      useOpeningBook('lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1')
    );
    
    // Assert
    expect(result.current.moves).toEqual([]);
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
    expect(result.current.level).toBe('early');
  });
});
```

**Green: フックの基本実装**

`packages/web/src/hooks/useOpeningBook.ts`:

```typescript
import { useState } from 'react';
import type { BookMove, LoadProgress, BookLevel } from '@/types/openingBook';

export function useOpeningBook(sfen: string) {
  const [moves, setMoves] = useState<BookMove[]>([]);
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState<LoadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [level, setLevel] = useState<BookLevel>('early');
  
  return {
    moves,
    loading,
    progress,
    error,
    level,
    loadMoreData: async (newLevel: BookLevel) => {
      // 後で実装
    }
  };
}
```

### Step 3-2: 自動初期化のテスト

**Red: マウント時の初期化テスト**

```typescript
it('should auto-initialize on mount', async () => {
  // Arrange
  const mockService = {
    initialize: vi.fn().mockResolvedValue(undefined),
    loadBook: vi.fn().mockResolvedValue(undefined),
  };
  
  // サービスをモック
  vi.mock('../services/openingBook', () => ({
    openingBook: mockService
  }));
  
  // Act
  const { result } = renderHook(() => useOpeningBook(''));
  
  // Wait for effect
  await vi.waitFor(() => {
    expect(mockService.initialize).toHaveBeenCalled();
    expect(mockService.loadBook).toHaveBeenCalledWith('early');
  });
});
```

**Green: 自動初期化の実装**

```typescript
import { useState, useEffect } from 'react';
import { openingBook } from '@/services/openingBook';

export function useOpeningBook(sfen: string) {
  const [moves, setMoves] = useState<BookMove[]>([]);
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState<LoadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [level, setLevel] = useState<BookLevel>('early');
  
  // 初期化
  useEffect(() => {
    openingBook.initialize()
      .then(() => openingBook.loadBook('early'))
      .catch(err => setError(err.message));
  }, []);
  
  return {
    moves,
    loading,
    progress,
    error,
    level,
    loadMoreData: async (newLevel: BookLevel) => {
      // 後で実装
    }
  };
}
```

## Phase 4: UIコンポーネント

### Step 4-1: 基本表示のテスト

**Red: コンポーネントの表示テスト**

`packages/web/src/components/OpeningBook/__tests__/OpeningBook.test.tsx`:

```typescript
import { render } from '@testing-library/react';
import { describe, it, expect } from 'vitest';
import { OpeningBook } from '../OpeningBook';

describe('OpeningBook', () => {
  it('should render title', () => {
    // Arrange & Act
    const { getByText } = render(<OpeningBook />);
    
    // Assert
    expect(getByText('定跡')).toBeInTheDocument();
  });
});
```

**Green: 最小限のコンポーネント**

`packages/web/src/components/OpeningBook/OpeningBook.tsx`:

```typescript
import React from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';

export function OpeningBook() {
  return (
    <Card>
      <CardHeader>
        <CardTitle>定跡</CardTitle>
      </CardHeader>
      <CardContent>
        {/* 後で実装 */}
      </CardContent>
    </Card>
  );
}
```

### Step 4-2: 定跡手の表示テスト

**Red: 定跡手リストのテスト**

```typescript
it('should display book moves', () => {
  // Arrange
  const mockUseOpeningBook = {
    moves: [
      { notation: '7g7f', evaluation: 50, depth: 10 },
      { notation: '2g2f', evaluation: 45, depth: 8 }
    ],
    loading: false,
    error: null,
    level: 'early' as const,
    progress: null,
    loadMoreData: vi.fn()
  };
  
  vi.mock('@/hooks/useOpeningBook', () => ({
    useOpeningBook: () => mockUseOpeningBook
  }));
  
  // Act
  const { getByText } = render(<OpeningBook />);
  
  // Assert
  expect(getByText('7g7f')).toBeInTheDocument();
  expect(getByText('評価: +50 深さ: 10')).toBeInTheDocument();
});
```

**Green: 定跡手の表示実装**

```typescript
import React from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { useOpeningBook } from '@/hooks/useOpeningBook';
import { useGameStore } from '@/stores/gameStore';

export function OpeningBook() {
  const sfen = useGameStore(state => state.getCurrentSfen?.() || '');
  const { moves, loading, error } = useOpeningBook(sfen);
  
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
        <div className="space-y-2">
          {moves.map((move, idx) => (
            <Button
              key={idx}
              variant="outline"
              className="w-full justify-between"
            >
              <span className="font-mono">{move.notation}</span>
              <span className="text-sm text-muted-foreground">
                評価: {formatEval(move.evaluation)} 深さ: {move.depth}
              </span>
            </Button>
          ))}
        </div>
      </CardContent>
    </Card>
  );
}
```

## テスト実行とリファクタリング

各フェーズの最後に必ず：

1. **全テストを実行**
   ```bash
   # Rust
   cargo test
   
   # TypeScript
   npm run test
   ```

2. **カバレッジ確認**
   ```bash
   npm run coverage
   ```

3. **リファクタリング検討**
   - 重複コードの削除
   - 命名の改善
   - 構造の整理

## 次のステップ

このガイドに従って実装を進めた後：

1. **統合テスト**: 各層を結合したE2Eテストを追加
2. **パフォーマンステスト**: 大量データでの動作確認
3. **エラーハンドリング**: 各種エラーケースの追加
4. **最適化**: メモリ使用量とロード時間の改善

## 参考資料

- [TDD Implementation Guide](../development/tdd-implementation-guide.md)
- [Opening Book Binary Format Specification](../reference/opening-book-binary-format.md)
- [Original Implementation Guide](./opening-book-implementation-guide.md)