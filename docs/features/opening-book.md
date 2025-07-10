# 定跡システム仕様書

## 概要

将棋アプリケーションの定跡システムは、プロ棋士の実戦譜を基にした定跡データベースを提供し、AI対戦時や学習モードで活用されます。

## システム構成

### 定跡データベース

定跡データは難易度別に5つのSQLiteデータベースファイルとして管理されています：

- `user_book_10K.db` - 初級者向け（10,000局）
- `user_book_30K.db` - 中級者向け（30,000局）
- `user_book_50K.db` - 中上級者向け（50,000局）
- `user_book_70K.db` - 上級者向け（70,000局）
- `user_book_100T.db` - 最上級者向け（100,000局以上）

### データ構造

各データベースには以下のテーブルが含まれます：

```sql
-- 定跡手テーブル
CREATE TABLE book_moves (
    sfen TEXT NOT NULL,           -- 局面のSFEN表記
    move TEXT NOT NULL,           -- 指し手（USI形式）
    count INTEGER NOT NULL,       -- この手が指された回数
    win_count INTEGER NOT NULL,   -- 勝利数
    PRIMARY KEY (sfen, move)
);

-- メタデータテーブル
CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

## 実装状況

### Rustコア (packages/rust-core)

- ✅ 定跡データベースの読み込み機能
- ✅ 局面検索と候補手取得
- ✅ WebAssemblyバインディング
- ✅ エラーハンドリング

### Webフロントエンド (packages/web)

- ✅ 定跡データベースの動的ロード
- ✅ 難易度別データベース選択
- ✅ AIエンジンとの統合
- ✅ 定跡手の表示機能

## API仕様

### Rust API

```rust
// 定跡データベースのロード
pub fn load_opening_book(data: &[u8]) -> Result<(), JsValue>

// 局面から定跡手を検索
pub fn search_opening_book(sfen: &str) -> Vec<OpeningMove>

// 定跡データベースのクリア
pub fn clear_opening_book()
```

### JavaScript API

```typescript
// 定跡の初期化と読み込み
async function initializeOpeningBook(difficulty: 'beginner' | 'intermediate' | 'advanced' | 'expert' | 'master'): Promise<void>

// 局面から定跡手を取得
function getOpeningMoves(sfen: string): OpeningMove[]

// 定跡手の型定義
interface OpeningMove {
  move: string;      // USI形式の指し手
  count: number;     // 出現回数
  winRate: number;   // 勝率（0.0-1.0）
}
```

## 統合ポイント

### AIエンジンとの統合

AIエンジンは定跡データベースを以下のように活用します：

1. **定跡フェーズ**: 序盤は定跡データベースから手を選択
2. **評価値補正**: 定跡に含まれる手に対して評価値ボーナスを付与
3. **学習モード**: 定跡手の解説と変化の表示

### 難易度設定との連携

- 初級: `user_book_10K.db` - 基本定跡のみ
- 中級: `user_book_30K.db` - 一般的な定跡
- 上級: `user_book_50K.db` - 幅広い定跡
- エキスパート: `user_book_70K.db` - 高度な定跡
- マスター: `user_book_100T.db` - 最新定跡を含む

## パフォーマンス最適化

- **遅延読み込み**: 定跡データベースは必要時にのみロード
- **メモリ効率**: Rust側でデータを保持し、必要な部分のみJSに転送
- **キャッシュ**: 頻繁にアクセスされる局面はメモリにキャッシュ

## 今後の拡張予定

- [ ] カスタム定跡の追加機能
- [ ] 定跡の学習・編集機能
- [ ] 定跡解説の表示
- [ ] 定跡からの離脱タイミングの最適化