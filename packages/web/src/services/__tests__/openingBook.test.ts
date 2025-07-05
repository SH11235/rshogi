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

  it('should initialize WASM module', async () => {
    // Arrange
    const service = new OpeningBookService();
    
    // Act
    await service.initialize();
    
    // Assert
    expect(service.isInitialized()).toBe(true);
  });

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

  it('should handle load errors', async () => {
    // Arrange
    const service = new OpeningBookService();
    await service.initialize();
    
    // fetchをエラーでモック
    global.fetch = vi.fn().mockRejectedValue(new Error('Network error'));
    
    // Act & Assert
    await expect(service.loadBook('early')).rejects.toThrow('Network error');
  });

  it('should find moves for position', async () => {
    // Arrange
    const service = new OpeningBookService();
    await service.initialize();
    
    // Act
    const moves = await service.findMoves('test-sfen');
    
    // Assert
    expect(moves).toEqual([]);
  });
});