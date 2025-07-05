import type { BookLevel, LoadProgress, BookMove } from '@/types/openingBook';

export type ProgressCallback = (progress: LoadProgress) => void;

export class OpeningBookService {
  private initialized = false;
  private reader: any = null;
  private currentLevel: BookLevel | null = null;
  
  async initialize(): Promise<void> {
    if (this.initialized) return;
    
    // 仮実装：後でWASMを読み込む
    this.reader = {}; // モックオブジェクト
    this.initialized = true;
  }
  
  isInitialized(): boolean {
    return this.initialized;
  }
  
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
  
  async findMoves(sfen: string): Promise<BookMove[]> {
    if (!this.initialized || !this.reader) return [];

    try {
      // 仮実装：後でWASMから取得
      const movesJson = this.reader.find_moves?.(sfen) || '[]';
      return JSON.parse(movesJson);
    } catch (error) {
      console.error('Failed to find moves:', error);
      return [];
    }
  }
}

export const openingBook = new OpeningBookService();