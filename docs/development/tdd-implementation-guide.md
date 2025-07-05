# TDD Implementation Guide - t-wada Style

このガイドでは、t-wada氏が提唱するTDD（テスト駆動開発）のスタイルに基づいた実装手順を説明します。

## TDDの基本原則

### 1. テストファースト
- **実装前に必ずテストを書く**
- テストが失敗することを確認してから実装を始める
- テストは仕様であり、設計の一部

### 2. 小さなステップ
- 一度に一つの小さな機能だけを実装
- 各ステップは数分で完了できる大きさに
- 複雑な機能は段階的に構築

### 3. Red-Green-Refactorサイクル
1. **Red**: 失敗するテストを書く
2. **Green**: テストを通す最小限の実装
3. **Refactor**: コードを整理（テストは通ったまま）

### 4. 三角測量
- 複数のテストケースから一般化を導く
- 最初は具体的な値でテスト
- 徐々に一般的な実装へ

### 5. 明白な実装
- 実装が自明な場合は直接書いてOK
- ただし、必ずテストファースト

## TDD実装の具体的な手順

### Step 1: TODOリストの作成
```
□ 機能1のテストを書く
□ 機能1を実装する
□ 機能1をリファクタリング
□ 機能2のテストを書く
□ 機能2を実装する
...
```

### Step 2: 最初のテストを書く
```typescript
// まず何もない状態でテストを書く
describe('OpeningBookReader', () => {
  it('should create instance', () => {
    // Arrange & Act
    const reader = new OpeningBookReader();
    
    // Assert
    expect(reader).toBeDefined();
  });
});
```

### Step 3: コンパイルエラーを解消
```typescript
// 最小限の実装でコンパイルを通す
export class OpeningBookReader {
  // 空の実装でOK
}
```

### Step 4: 次の機能のテスト
```typescript
it('should load binary data', async () => {
  // Arrange
  const reader = new OpeningBookReader();
  const testData = new Uint8Array([1, 2, 3]);
  
  // Act
  const result = await reader.loadData(testData);
  
  // Assert
  expect(result).toBe('Loaded 0 positions'); // 最初は0でOK
});
```

### Step 5: 段階的な実装
```typescript
// テストを通す最小限の実装
async loadData(data: Uint8Array): Promise<string> {
  return 'Loaded 0 positions';
}
```

## アンチパターンの回避

### ❌ 避けるべきこと
- テストなしで実装を進める
- 大きなステップで進める
- リファクタリングを後回しにする
- テストの意図が不明確

### ✅ 推奨される実践
- 常にテストファースト
- 5分以内で完了するステップ
- こまめなリファクタリング
- テストが仕様書になるような命名

## 実装例：定跡機能のTDD

### 1. TODOリスト作成
```
□ Rust: OpeningBookReaderが作成できる
□ Rust: 空のバイナリデータを読み込める
□ Rust: 位置情報なしを返せる
□ Rust: 圧縮データを解凍できる
□ Rust: 1つの位置を読み込める
□ Rust: 複数の位置を読み込める
□ Rust: SFENから位置を検索できる
□ TypeScript: サービスを初期化できる
□ TypeScript: WASMを読み込める
□ TypeScript: データをダウンロードできる
□ TypeScript: 進捗を通知できる
□ React: フックが初期化される
□ React: 読み込み状態を表示できる
□ React: 定跡手を表示できる
□ React: クリックで指し手を実行できる
```

### 2. 各ステップの実装パターン

#### Rustのテスト駆動開発
```rust
// Step 1: Red - 失敗するテスト
#[test]
fn test_create_reader() {
    let reader = OpeningBookReader::new();
    assert_eq!(reader.position_count(), 0);
}

// Step 2: Green - 最小限の実装
pub struct OpeningBookReader {
    positions: HashMap<u64, Vec<BookMove>>,
}

impl OpeningBookReader {
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
        }
    }
    
    pub fn position_count(&self) -> usize {
        self.positions.len()
    }
}

// Step 3: Refactor - 必要に応じて整理
```

#### TypeScriptのテスト駆動開発
```typescript
// Step 1: Red - 失敗するテスト
describe('OpeningBookService', () => {
  it('should initialize without error', async () => {
    const service = new OpeningBookService();
    await expect(service.initialize()).resolves.not.toThrow();
  });
});

// Step 2: Green - 最小限の実装
export class OpeningBookService {
  async initialize(): Promise<void> {
    // 空の実装でテストを通す
  }
}

// Step 3: Refactor - 次のテストに備えて整理
```

#### Reactコンポーネントのテスト駆動開発
```typescript
// Step 1: Red - 表示のテスト
it('should show loading state', () => {
  const { getByText } = render(<OpeningBook />);
  expect(getByText('読み込み中')).toBeInTheDocument();
});

// Step 2: Green - 最小限のコンポーネント
export function OpeningBook() {
  return <div>読み込み中</div>;
}

// Step 3: 次のテスト - 定跡手の表示
it('should show book moves', () => {
  const moves = [
    { notation: '7g7f', evaluation: 50, depth: 10 }
  ];
  const { getByText } = render(<OpeningBook moves={moves} />);
  expect(getByText('7g7f')).toBeInTheDocument();
});
```

## テスト戦略

### 1. ユニットテスト
- 各関数・メソッドの動作を検証
- 依存関係はモック化
- 高速で安定した実行

### 2. 統合テスト
- 複数のコンポーネントの連携を検証
- 実際のWASMモジュールを使用
- E2Eに近い動作確認

### 3. E2Eテスト
- ユーザーシナリオ全体を検証
- ブラウザでの実際の動作
- 重要なユースケースのみ

## リファクタリングのタイミング

### いつリファクタリングするか
1. テストが全て通った後
2. 重複が見つかったとき（DRY原則）
3. コードの意図が不明確なとき
4. 新機能追加の前準備として

### リファクタリングの例
```typescript
// Before: 重複のあるコード
async loadEarlyData() {
  const response = await fetch('/data/early.bin.gz');
  const data = await response.arrayBuffer();
  return this.reader.load(new Uint8Array(data));
}

async loadStandardData() {
  const response = await fetch('/data/standard.bin.gz');
  const data = await response.arrayBuffer();
  return this.reader.load(new Uint8Array(data));
}

// After: 共通化
async loadData(level: BookLevel) {
  const response = await fetch(`/data/${level}.bin.gz`);
  const data = await response.arrayBuffer();
  return this.reader.load(new Uint8Array(data));
}
```

## 実装の進め方

### 1. 垂直スライス
- 機能を縦に切って、エンドツーエンドで動くものを作る
- 例：「初期局面の定跡を1つ表示する」を完成させる

### 2. 水平展開
- 基本機能ができたら、横に広げる
- 例：複数の定跡表示、評価値表示、深さ表示

### 3. 品質向上
- パフォーマンス最適化
- エラーハンドリング
- ユーザビリティ向上

## チェックリスト

実装を始める前に：
- [ ] TODOリストは作成したか？
- [ ] 最初のテストは書いたか？
- [ ] テストは失敗するか？

実装中：
- [ ] テストファーストを守っているか？
- [ ] ステップは十分小さいか？
- [ ] こまめにコミットしているか？

実装後：
- [ ] 全てのテストが通るか？
- [ ] リファクタリングの余地はないか？
- [ ] 次のステップは明確か？

## 参考資料

- 「テスト駆動開発」 Kent Beck著
- t-wada氏の講演資料・ブログ記事
- 「実践テスト駆動開発」 Freeman & Pryce著