# 難易度別定跡ファイル読み込み仕様

## 概要

AIの難易度に応じて、異なる定跡ファイルを読み込む。初心者向けには軽量なJSONファイル、上級者向けには詳細なバイナリファイルを使用する。

## 利用可能な定跡ファイル

### JSON形式
- **beginner-openings.json** (29KB)
  - 7,467エントリ
  - 初心者向けに最適化
  - 即座に読み込み可能

### バイナリ形式（.bin.gz）
- **opening_book_early.bin.gz** (13MB)
  - 序盤10手までの定跡
  - 基本的な戦型をカバー
  
- **opening_book_standard.bin.gz** (24MB)
  - 標準的な定跡データ
  - web版と同じ内容
  
- **opening_book_web.bin.gz** (24MB)
  - Web用に最適化
  - 20手までの定跡
  
- **opening_book_full.bin.gz** (67MB)
  - 完全な定跡データベース
  - すべての手数を含む
  
- **opening_book_tournament.bin.gz** (8.6MB)
  - トーナメント用
  - 厳選された定跡

## 難易度別読み込み仕様

### 1. Beginner（初心者）
```typescript
// ファイル: beginner-openings.json
// サイズ: 29KB
// 特徴: 即座に読み込み、基本的な定跡のみ
if (message.difficulty === "beginner") {
    const response = await fetch("/data/beginner-openings.json");
    const data = await response.json();
    engine.loadOpeningBook(data.entries);
}
```
- **フォールバック**: generateMainOpenings()で基本定跡を生成

### 2. Intermediate（中級者）
```typescript
// ファイル: opening_book_early.bin.gz
// サイズ: 13MB
// 特徴: 序盤10手まで、主要な戦型をカバー
if (message.difficulty === "intermediate") {
    const loader = new OpeningBookLoader("/data");
    await loader.loadSingleFile("opening_book_early.bin.gz");
    engine.setOpeningBook(loader.getOpeningBook());
}
```
- **理由**: 中級者は基本的な定跡を知っていれば十分
- **読み込み時間**: 約1-2秒

### 3. Advanced（上級者）
```typescript
// ファイル: opening_book_standard.bin.gz
// サイズ: 24MB
// 特徴: 標準的な定跡、20手まで
if (message.difficulty === "advanced") {
    const loader = new OpeningBookLoader("/data");
    await loader.loadSingleFile("opening_book_standard.bin.gz");
    engine.setOpeningBook(loader.getOpeningBook());
}
```
- **理由**: より深い定跡知識が必要
- **読み込み時間**: 約2-3秒

### 4. Expert（エキスパート）
```typescript
// ファイル: opening_book_full.bin.gz
// サイズ: 67MB
// 特徴: 完全な定跡データベース
if (message.difficulty === "expert") {
    const loader = new OpeningBookLoader("/data");
    await loader.loadSingleFile("opening_book_full.bin.gz");
    engine.setOpeningBook(loader.getOpeningBook());
}
```
- **理由**: 最高レベルのAIには完全な定跡知識が必要
- **読み込み時間**: 約5-7秒
- **メモリ使用量**: 約150-200MB

## 実装詳細

### OpeningBookLoader の拡張
```typescript
class OpeningBookLoader {
    // 既存のメソッド...
    
    // 単一ファイルの読み込み（バイナリ形式用）
    async loadSingleFile(filename: string): Promise<void> {
        const url = `${this.baseUrl}/${filename}`;
        const response = await fetch(url);
        const data = await response.arrayBuffer();
        
        // WASM経由でバイナリデータを読み込み
        const reader = new OpeningBookReaderWasm();
        reader.load_data(new Uint8Array(data));
        
        // reader から OpeningBook インスタンスへ変換
        this.convertToOpeningBook(reader);
    }
}
```

### エラーハンドリング
```typescript
try {
    // 難易度に応じた定跡読み込み
} catch (error) {
    console.error(`定跡読み込みエラー (${difficulty}):`, error);
    
    // フォールバック処理
    if (difficulty === "beginner" || difficulty === "intermediate") {
        // 基本定跡を生成
        const openingData = generateMainOpenings();
        engine.loadOpeningBook(openingData);
    } else {
        // 上級者以上は定跡なしで続行
        console.warn("定跡なしで続行します");
    }
}
```

## パフォーマンス考慮事項

### メモリ使用量
- Beginner: 約1MB（JSON）
- Intermediate: 約30MB（解凍後）
- Advanced: 約50MB（解凍後）
- Expert: 約150-200MB（解凍後）

### 読み込み時間
- Beginner: 即座（< 100ms）
- Intermediate: 1-2秒
- Advanced: 2-3秒
- Expert: 5-7秒

### 最適化案
1. **プログレッシブ読み込み**: Expert難易度では最初の一部だけ読み込み、残りはバックグラウンドで
2. **キャッシュ**: IndexedDBに解凍済みデータを保存
3. **Web Worker**: メインスレッドをブロックしない

## 今後の拡張

### カスタム難易度
ユーザーが定跡の深さを選択できるオプション：
- 定跡なし
- 序盤のみ（10手まで）
- 標準（20手まで）
- 完全（すべて）

### 戦型別定跡
特定の戦型に特化した定跡ファイル：
- 居飛車専用
- 振り飛車専用
- 相振り飛車専用