# 定跡実装の整理

## 現在の実装状況

### 1. コア機能 (packages/core/src/ai/)

#### 主要ファイル
- **openingBook.ts**: 定跡データベースの基本クラス
  - 局面をSFEN形式（手数除外）で管理
  - 重み付きランダム選択
  - メモリ管理機能（estimateMemoryUsage, removeDeepEntries）

- **openingBookLoader.ts**: 定跡データの非同期ローダー【実際に使用中】
  - 分割ファイルの段階的読み込み
  - gzip圧縮ファイル対応
  - メモリ制限（200MB）と自動削減機能
  - packages/web/src/workers/aiWorker.tsで使用

- **binaryOpeningBookLoader.ts**: MessagePack形式対応ローダー【未使用】
  - バイナリ形式での効率的なデータ保存
  - JSON形式との後方互換性
  - 将来的なMessagePack移行用に実装済み

- **openingBookSearch.ts**: 効率的な検索アルゴリズム【未使用】
  - ハッシュベースの高速検索
  - LRUキャッシュ実装
  - 将来的な高速化用に実装済み

#### 型定義
- **binaryOpeningTypes.ts**: バイナリ形式の型定義
  - 圧縮された手の表現（MoveBinary）
  - ビットフラグによる効率的な情報格納

### 2. Web実装 (packages/web/)

#### AIワーカー統合
- **aiWorker.ts**: 定跡データベースの初期化と使用
  - 初級者向け: beginner-openings.json
  - 中級以上: 分割された大容量定跡（openings-XXX.gzip）

#### ゲームストア
- **gameStore.ts**: AI実行フローの修正
  - executeAIMove: 定跡使用後のゲームフロー継続処理
  - isAIThinkingフラグの適切な管理

### 3. データファイル

#### 生成済みデータ
- **/data/openings/**: 
  - standard-openings.gzip: 標準定跡（深さ3まで）
  - openings-000.gzip～openings-029.gzip: YaneuraOu定跡（80万件）
  - index.json: ファイル一覧とメタデータ

- **/data/beginner-openings.json**: 初級者向け定跡（29KB）

### 4. スクリプト (packages/core/scripts/)

#### 現在使用中
- **convertOpeningBook.ts**: YaneuraOu DB形式から変換
- **generateBeginnerOpenings.ts**: 初級者向け定跡生成
- **generateStandardOpenings.ts**: 標準定跡生成
- **addStandardOpenings.ts**: 標準定跡を既存データに追加
- **convertToMessagePack.ts**: JSONからMessagePack形式への変換

#### テスト・デバッグ用（削除候補）
- **testOpeningLookup.ts**: 定跡検索のテスト
- **debugOpeningBook.ts**: 定跡動作の単体テスト

### 5. 未使用・削除候補ファイル

#### 未参照ファイル
- **packages/core/src/ai/beginnerOpenings.ts**: 使用されていない
- **packages/core/src/ai/openingBookLoaderNode.ts**: 使用されていない
- **packages/core/src/ai/yaneuraouParser.ts**: convertOpeningBook.tsのみで参照

#### デバッグ用ファイル
- **packages/web/public/debug-opening-book.html**: デバッグ用HTML
- **tests/ai-opening-book-freeze.spec.ts**: 問題解決後は不要
- **user_book1.db**: テスト用データベース

### 6. ビルド設定

#### Vite設定
- gzip→gzip拡張子変更によるViteの自動解凍回避
- 定跡データのコピー処理（build時）

## 現在の定跡データ読み込みフロー

1. **初級者（beginner）**:
   - `/data/beginner-openings.json`を直接読み込み
   - 読み込み失敗時は`generateMainOpenings()`でフォールバック

2. **中級者以上（intermediate, advanced, expert）**:
   - `OpeningBookLoader`を使用して分割ファイルを段階的に読み込み
   - 初回は5ファイル（preloadCount: 5）を読み込み
   - 残りはバックグラウンドで非同期読み込み
   - 進捗状況をメインスレッドに通知

## 削除可能なファイル

1. **既に存在しないファイル**:
   - packages/core/src/ai/beginnerOpenings.ts
   - packages/core/src/ai/openingBookLoaderNode.ts

2. **削除候補**:
   - packages/web/public/debug-opening-book.html（デバッグ用）
   - packages/core/scripts/testOpeningLookup.ts（テスト用）
   - packages/core/scripts/debugOpeningBook.ts（デバッグ用）
   - tests/ai-opening-book-freeze.spec.ts（問題解決済み）
   - user_book1.db（テスト用データ）

3. **移動候補**:
   - packages/core/src/ai/yaneuraouParser.ts → packages/core/scripts/（スクリプト専用）

## 保持すべきファイル

1. **主要実装ファイル**:
   - openingBook.ts, openingBookLoader.ts（実際に使用中）
   - binaryOpeningBookLoader.ts, openingBookSearch.ts（将来の拡張用）
   - binaryOpeningTypes.ts（型定義）

2. **データ生成スクリプト**:
   - convertOpeningBook.ts, generateBeginnerOpenings.ts
   - generateStandardOpenings.ts, addStandardOpenings.ts
   - convertToMessagePack.ts

3. **生成済みデータ**:
   - /data/openings/内の全gzipファイル
   - /data/beginner-openings.json

## 今後の改善案

1. **パフォーマンス最適化**:
   - openingBookSearch.tsの活用（高速検索）
   - MessagePack形式への移行（ファイルサイズ60-70%削減）
   - WebWorkerでの並列処理強化

2. **機能拡張**:
   - 定跡エディタUIの実装
   - ユーザー定跡の追加・編集機能
   - 定跡の統計情報表示