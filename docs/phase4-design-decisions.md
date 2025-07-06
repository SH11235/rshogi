# Phase 4: 設計判断と詳細仕様

## 1. 統一されたバイナリ形式の採用

### 決定事項
- 全難易度でバイナリ形式（.bin.gz）を使用
- beginner-openings.jsonは使用しない

### 理由
- メンテナンス性の向上（単一フォーマット）
- 実装の一貫性
- ファイルサイズの最適化（tournament版は8.6MB）

## 2. インターフェース設計

### Move形式の統一
```typescript
// shogi-core内部では文字列表記を使用
type MoveNotation = string; // "7g7f"

// OpeningMoveインターフェース
interface OpeningMove {
    move: Move;        // 内部的にはMoveオブジェクトに変換
    notation: string;  // 元の文字列表記を保持
    weight: number;    // 評価値をweightとして統一
    depth?: number;    // 探索深さ（オプション）
}
```

### 変換戦略
- バイナリデータ読み込み時に文字列→Moveオブジェクト変換
- 既存のmoveServiceを活用してnotationからMoveを生成

## 3. SFEN位置ハッシュ戦略

### 実装方針
```typescript
// シンプルなSFENベースのキー生成
function generatePositionKey(position: PositionState): string {
    const { board, hands, currentPlayer } = position;
    // 手数を除外したSFEN生成
    return `${boardToSfen(board)} ${currentPlayer} ${handsToSfen(hands)}`;
}
```

### 理由
- Zobristハッシュの複雑性を回避
- デバッグとテストの容易性
- JavaScriptでの実装効率

## 4. メモリ管理戦略

### 制限値
- グローバル制限: 200MB（全定跡データ合計）
- 難易度切り替え時に前のデータをクリア

### 実装
```typescript
class OpeningBook {
    private memoryUsage = 0;
    private readonly MAX_MEMORY = 200 * 1024 * 1024; // 200MB
    
    addEntry(entry: OpeningEntry): boolean {
        const entrySize = this.estimateSize(entry);
        if (this.memoryUsage + entrySize > this.MAX_MEMORY) {
            return false; // メモリ制限超過
        }
        // エントリ追加処理
        this.memoryUsage += entrySize;
        return true;
    }
    
    clear(): void {
        this.positions.clear();
        this.memoryUsage = 0;
    }
}
```

## 5. エラーハンドリング仕様

### フォールバック戦略
- beginner/intermediate: generateMainOpenings()にフォールバック
- advanced/expert: 定跡なしで続行（通常のAI探索）

### エラーケース
1. ネットワークエラー → フォールバック
2. 不正なデータ形式 → エラーログ + フォールバック
3. メモリ不足 → 部分的な読み込みで続行

## 6. パフォーマンス要件

### 目標値
| 項目 | 目標 | 備考 |
|------|------|------|
| 定跡検索時間 | < 1ms | Map検索のため高速 |
| 初期読み込み（beginner） | < 2秒 | 8.6MB |
| 初期読み込み（expert） | < 7秒 | 67MB |
| メモリ使用量 | < 200MB | 全データ合計 |

## 7. バイナリ解析の実装詳細

### MoveEncoderの実装
```typescript
class MoveEncoder {
    // 16ビットエンコーディング
    // bit 0-3: from column (0-8)
    // bit 4-7: from row (0-8)
    // bit 8-11: to column (0-8)
    // bit 12-15: to row (0-8)
    
    static decode(encoded: number): string {
        const fromCol = 9 - (encoded & 0xF);
        const fromRow = 1 + ((encoded >> 4) & 0xF);
        const toCol = 9 - ((encoded >> 8) & 0xF);
        const toRow = 1 + ((encoded >> 12) & 0xF);
        
        return `${fromCol}${String.fromCharCode(96 + fromRow)}${toCol}${String.fromCharCode(96 + toRow)}`;
    }
}
```

## 8. AIEngine統合仕様

### 定跡チェックのタイミング
```typescript
async calculateBestMove(...): Promise<Move> {
    // 1. 定跡チェック（20手未満）
    if (this.config.useOpeningBook && moveHistory.length < 20) {
        const openingMove = this.checkOpeningBook(position);
        if (openingMove) return openingMove;
    }
    
    // 2. 終盤データベースチェック
    if (this.config.useEndgameDatabase) {
        const endgameMove = this.checkEndgameDatabase(position);
        if (endgameMove) return endgameMove;
    }
    
    // 3. 通常の探索
    return this.search(position);
}
```

### useOpeningBookフラグ
- 全難易度でデフォルトtrue
- ユーザー設定で無効化可能

## 9. テストデータ戦略

### 単体テスト用データ
```typescript
// テスト用の最小バイナリデータ生成
function createTestBinaryData(): Uint8Array {
    const buffer = new ArrayBuffer(22); // header(16) + 1 move(6)
    const view = new DataView(buffer);
    
    // 初期局面のハッシュ（仮）
    view.setBigUint64(0, 0x123456789ABCDEFn, true);
    view.setUint16(8, 1, true); // 1手
    
    // 7g7f の手
    view.setUint16(16, encodeMove("7g7f"), true);
    view.setInt16(18, 50, true); // 評価値
    view.setUint8(20, 20); // 深さ
    
    return new Uint8Array(buffer);
}
```

## 10. 移行計画

### 段階的実装
1. Phase 4.1: OpeningBook基本実装とテスト
2. Phase 4.2: バイナリローダー実装
3. Phase 4.3: AIEngine統合
4. Phase 4.4: aiWorker有効化と統合テスト

### 既存コードへの影響
- OpeningBookServiceは将来的にshogi-coreのものに置き換え
- 現時点ではWeb専用の実装として残す