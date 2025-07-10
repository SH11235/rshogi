# 定跡機能の実装計画書（TDDアプローチ）

## 概要
rust-core の `OpeningBookReaderWasm` を利用して、Web フロントエンドの `OpeningBookService` に定跡読み込み機能を実装する。

## 現状分析

### 完成している部分
- ✅ Rust/WASM の `OpeningBookReaderWasm` クラス（完全実装済み）
- ✅ バイナリ形式の定跡ファイル（.bin.gz）の生成と配置
- ✅ UI コンポーネント（OpeningBook.tsx）とフック（useOpeningBook.ts）
- ✅ AI ワーカーでの JSON 形式定跡の統合

### 未実装の部分
- ❌ `OpeningBookService` の WASM 統合（現在はモック実装）
- ❌ WASM モジュールの初期化処理
- ❌ バイナリ形式定跡ファイルの読み込み

## TDD による実装計画

### Phase 1: WASM モジュールの初期化（RED → GREEN → REFACTOR）

#### 1.1 RED: 初期化テストの作成
```typescript
// openingBook.test.ts
describe('OpeningBookService - WASM Initialization', () => {
  it('should initialize WASM module on first call', async () => {
    const service = new OpeningBookService();
    expect(service.isInitialized()).toBe(false);
    
    await service.initialize();
    
    expect(service.isInitialized()).toBe(true);
    expect(service['reader']).toBeDefined();
    expect(service['reader']).toHaveProperty('load_data');
    expect(service['reader']).toHaveProperty('find_moves');
  });

  it('should not reinitialize if already initialized', async () => {
    const service = new OpeningBookService();
    await service.initialize();
    const firstReader = service['reader'];
    
    await service.initialize();
    
    expect(service['reader']).toBe(firstReader);
  });
});
```

#### 1.2 GREEN: 最小限の実装
- WASM モジュールのインポートと初期化
- `OpeningBookReaderWasm` インスタンスの作成

#### 1.3 REFACTOR: コードの改善
- エラーハンドリングの追加
- 型定義の明確化

### Phase 2: 定跡データの読み込み（RED → GREEN → REFACTOR）

#### 2.1 RED: データ読み込みテストの作成
```typescript
describe('OpeningBookService - Data Loading', () => {
  it('should load early level book data', async () => {
    const service = new OpeningBookService();
    const onProgress = vi.fn();
    
    await service.loadBook('early', onProgress);
    
    expect(onProgress).toHaveBeenCalledWith({
      phase: 'downloading',
      loaded: expect.any(Number),
      total: expect.any(Number),
    });
    expect(onProgress).toHaveBeenCalledWith({
      phase: 'decompressing',
      loaded: 100,
      total: 100,
    });
    expect(service['currentLevel']).toBe('early');
  });

  it('should not reload same level book', async () => {
    const service = new OpeningBookService();
    await service.loadBook('early');
    const fetchSpy = vi.spyOn(global, 'fetch');
    
    await service.loadBook('early');
    
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
```

#### 2.2 GREEN: データ読み込みの実装
- fetch API によるバイナリデータの取得
- WASM の `load_data` メソッドへのデータ渡し
- プログレスコールバックの実装

#### 2.3 REFACTOR: 改善
- メモリ効率の最適化
- エラーリトライ機構の追加

### Phase 3: 手の検索機能（RED → GREEN → REFACTOR）

#### 3.1 RED: 手検索テストの作成
```typescript
describe('OpeningBookService - Move Finding', () => {
  it('should find moves for initial position', async () => {
    const service = new OpeningBookService();
    await service.loadBook('early');
    const initialSfen = 'lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1';
    
    const moves = await service.findMoves(initialSfen);
    
    expect(moves).toBeInstanceOf(Array);
    expect(moves.length).toBeGreaterThan(0);
    expect(moves[0]).toHaveProperty('notation');
    expect(moves[0]).toHaveProperty('evaluation');
    expect(moves[0]).toHaveProperty('depth');
  });

  it('should return empty array for unknown position', async () => {
    const service = new OpeningBookService();
    await service.loadBook('early');
    
    const moves = await service.findMoves('unknown_position');
    
    expect(moves).toEqual([]);
  });
});
```

#### 3.2 GREEN: 手検索の実装
- WASM の `find_moves` メソッドの呼び出し
- JSON 文字列のパースと型変換

#### 3.3 REFACTOR: 改善
- 型安全性の向上
- キャッシュ機構の検討

### Phase 4: AI サービスとの統合（RED → GREEN → REFACTOR）

#### 4.1 RED: AI 統合テストの作成
```typescript
describe('AI Opening Book Integration', () => {
  it('should use opening book moves before search', async () => {
    const mockOpeningBook = {
      findMoves: vi.fn().mockResolvedValue([
        { notation: '7g7f', evaluation: 50, depth: 20 }
      ])
    };
    
    const result = await ai.getBestMove(position, { 
      openingBook: mockOpeningBook 
    });
    
    expect(mockOpeningBook.findMoves).toHaveBeenCalledWith(position.toSfen());
    expect(result.move).toBe('7g7f');
    expect(result.source).toBe('opening_book');
  });
});
```

#### 4.2 GREEN: AI ワーカーへの統合
- AI ワーカーでの OpeningBookService 利用
- 定跡手の優先的な返却

#### 4.3 REFACTOR: 改善
- パフォーマンスの最適化
- メモリ使用量の削減

## 実装スケジュール

### Week 1: 基盤整備
- [ ] テスト環境のセットアップ
- [ ] Phase 1 の実装（WASM 初期化）

### Week 2: コア機能
- [ ] Phase 2 の実装（データ読み込み）
- [ ] Phase 3 の実装（手の検索）

### Week 3: 統合と最適化
- [ ] Phase 4 の実装（AI 統合）
- [ ] パフォーマンステスト
- [ ] 統合テスト

## テスト戦略

### 単体テスト
- 各メソッドの正常系・異常系テスト
- モックを使用した依存関係の分離

### 統合テスト
- WASM モジュールとの実際の連携テスト
- エンドツーエンドの動作確認

### パフォーマンステスト
- 大容量定跡ファイルの読み込み時間
- メモリ使用量の監視
- 検索速度の計測

## リスクと対策

### リスク 1: WASM の初期化失敗
- **対策**: フォールバック処理の実装、エラーメッセージの明確化

### リスク 2: メモリ不足
- **対策**: 段階的な読み込み、不要データの解放

### リスク 3: ブラウザ互換性
- **対策**: WebAssembly サポートの確認、ポリフィルの検討

## 成功基準

1. **機能要件**
   - WASM モジュールが正常に初期化される
   - 3 レベルの定跡データが読み込める
   - SFEN から正しく手が検索できる
   - AI が定跡を使用して着手する

2. **非機能要件**
   - 初期化時間: 1 秒以内
   - データ読み込み: 5 秒以内（full レベル）
   - 手の検索: 10ms 以内
   - メモリ使用量: 200MB 以下

## 参考資料

- t-wada 氏の TDD 実践ガイド
- Martin Fowler のリファクタリング原則
- WebAssembly ベストプラクティス