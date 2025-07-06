# Phase 4: shogi-core 定跡機能実装要件定義書

## 概要

shogi-coreパッケージにAIの定跡機能を実装する。これによりaiWorker.tsから定跡データを読み込み、AIが序盤で定跡手を指せるようになる。

## 実装対象

### 1. OpeningBook クラス

#### 目的
定跡データを保持し、現在の局面に対する定跡手を検索する。

#### インターフェース
```typescript
interface OpeningEntry {
    position: string;      // SFEN形式（手数なし）
    moves: OpeningMove[];  // 候補手のリスト
    depth: number;        // 探索深さ
}

interface OpeningMove {
    move: Move;           // 指し手
    weight: number;       // 重み（選択確率）
    name?: string;        // 戦型名（例：「矢倉」）
    comment?: string;     // コメント
}

class OpeningBook {
    constructor();
    
    // 定跡エントリを追加
    addEntry(entry: OpeningEntry): void;
    
    // 局面から定跡手を検索
    findMoves(position: PositionState): OpeningMove[];
    
    // メモリ使用量を取得
    getMemoryUsage(): number;
    
    // エントリ数を取得
    size(): number;
    
    // 全データをクリア
    clear(): void;
}
```

#### 実装詳細
- SFENからキーを生成（盤面＋持ち駒＋手番、手数は除外）
- Map<string, OpeningMove[]>でデータを保持
- 重み付きランダム選択をサポート

### 2. OpeningBookLoader クラス

#### 目的
圧縮された定跡データを段階的に読み込む。

#### インターフェース
```typescript
interface LoaderConfig {
    preloadCount?: number;    // 初期読み込みファイル数
    onProgress?: (progress: LoadProgress) => void;
}

interface LoadProgress {
    loaded: number;          // 読み込み済みエントリ数
    total: number;          // 全エントリ数（推定）
    phase: 'loading' | 'processing' | 'complete';
}

class OpeningBookLoader {
    constructor(baseUrl: string);
    
    // 初期化（初期ファイルを読み込み）
    async initialize(config?: LoaderConfig): Promise<void>;
    
    // バックグラウンドで残りを読み込み
    async loadInBackground(): Promise<void>;
    
    // OpeningBookインスタンスを取得
    getOpeningBook(): OpeningBook;
    
    // 読み込みを中断
    abort(): void;
}
```

#### 実装詳細
- `/data/openings/index.json`からファイルリストを取得
- gzip圧縮されたJSONファイルを読み込み（pako使用）
- メモリ制限（200MB）を超えたら読み込み停止
- Web Workerでのバックグラウンド読み込み対応

### 3. generateMainOpenings 関数

#### 目的
ファイル読み込みが失敗した場合のフォールバック定跡を生成。

#### インターフェース
```typescript
function generateMainOpenings(): OpeningEntry[];
```

#### 実装詳細
- 基本的な定跡（矢倉、四間飛車、中飛車、相掛かりなど）
- 各戦型で5〜10手程度の変化
- 合計100〜200エントリ程度

### 4. AIEngine クラスの拡張

#### 追加メソッド
```typescript
class AIEngine {
    // 既存のプロパティ・メソッド...
    
    private openingBook?: OpeningBook;
    
    // 定跡データ配列を読み込み
    loadOpeningBook(entries: OpeningEntry[]): void;
    
    // OpeningBookインスタンスを設定
    setOpeningBook(book: OpeningBook): void;
    
    // calculateBestMove内で定跡をチェック
    private checkOpeningBook(position: PositionState): Move | null;
}
```

#### 実装詳細
- AIConfigに`useOpeningBook: boolean`を追加（デフォルトtrue）
- 手数20手未満の局面で定跡をチェック
- 定跡手が見つかれば即座に返す
- 見つからない場合は通常の探索を実行

## データフォーマット

### 定跡エントリJSON
```json
{
  "entries": [
    {
      "position": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -",
      "moves": [
        {
          "move": { "from": [7, 7], "to": [7, 6], "piece": "Pawn" },
          "weight": 100,
          "name": "居飛車"
        },
        {
          "move": { "from": [2, 7], "to": [2, 6], "piece": "Pawn" },
          "weight": 80,
          "name": "振り飛車"
        }
      ],
      "depth": 20
    }
  ]
}
```

### index.json（ファイルリスト）
```json
{
  "files": [
    { "name": "openings-001.json.gzip", "entries": 1000 },
    { "name": "openings-002.json.gzip", "entries": 1000 }
  ],
  "totalEntries": 30000
}
```

## エクスポート

`packages/core/src/index.ts`に追加：
```typescript
export { OpeningBook } from './ai/openingBook';
export { OpeningBookLoader } from './ai/openingBookLoader';
export { generateMainOpenings } from './ai/openingData';
export type { OpeningEntry, OpeningMove } from './ai/openingBook';
```

## テスト要件

### 単体テスト
1. OpeningBookクラス
   - エントリの追加と検索
   - SFENキー生成の正確性
   - メモリ使用量計算

2. OpeningBookLoaderクラス
   - ファイル読み込みの成功/失敗
   - プログレスコールバック
   - メモリ制限の遵守

3. generateMainOpenings関数
   - 有効な定跡データの生成
   - 最低限のエントリ数確保

4. AIEngine統合
   - 定跡手の優先的な選択
   - 定跡なし時の通常探索へのフォールバック

### 統合テスト
- aiWorker.tsからの完全な動作確認
- 難易度別の定跡読み込み
- メモリ使用量とパフォーマンス

## 実装優先順位

1. **高優先度**
   - OpeningBookクラス（基盤）
   - AIEngineへの統合
   - OpeningBookLoaderの基本機能

2. **中優先度**
   - generateMainOpenings関数
   - バックグラウンド読み込み
   - プログレス通知

3. **低優先度**
   - 詳細なエラーハンドリング
   - メモリ最適化
   - 追加の定跡データ

## 成功基準

1. **機能要件**
   - aiWorker.tsの定跡関連コードが動作する
   - 全難易度で定跡が使用される
   - フォールバック機構が正常に動作

2. **性能要件**
   - 初期読み込み: 3秒以内
   - 定跡検索: 1ms以内
   - メモリ使用量: 200MB以下

3. **品質要件**
   - 全テストケースが通過
   - TypeScript型安全性の確保
   - エラー時の適切なフォールバック