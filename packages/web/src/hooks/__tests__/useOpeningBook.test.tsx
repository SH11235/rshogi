import { renderHook } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
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

  it('should auto-initialize on mount', async () => {
    // Arrange
    const mockService = {
      initialize: vi.fn().mockResolvedValue(undefined),
      loadBook: vi.fn().mockResolvedValue(undefined),
    };
    
    // サービスをモック
    vi.mock('@/services/openingBook', () => ({
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
});