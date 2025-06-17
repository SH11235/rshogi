# E2E Testing Guide

このプロジェクトでは、Microsoft公式のPlaywright MCPとPlaywrightを使用してE2Eテストを実装しています。

## テスト構成

### テストの種類

1. **統合テスト** (`src/__tests__/integration/`)
   - React Testing Libraryを使用
   - コンポーネント間の相互作用をテスト
   - 実際のDOMなしでの高速テスト

2. **E2Eテスト** (`e2e/`)
   - Playwrightを使用
   - 実際のブラウザでの動作をテスト
   - ユーザー体験の包括的な検証

### setup

```
# Playwrightのインストール
npm install
# Playwrightのブラウザをインストール
npx playwright install-deps
npx playwright install
# PlaywrightのMCPをインストール
npx playwright install-mcp
```


### E2Eテスト実行

```bash
# 開発サーバーを起動
npm run dev

# 別ターミナルでE2Eテストを実行
npm run test:e2e

# UIモードでテストを実行（デバッグ用）
npm run test:e2e:ui

# 特定のテストファイルのみ実行
npx playwright test e2e/basic-gameplay.spec.ts

# 特定のブラウザでのみ実行
npx playwright test --project=chromium
```

### CI環境での実行

GitHub ActionsでE2Eテストが自動実行されます：

- **トリガー**: `packages/web/` 配下のファイル変更時
- **ブラウザ**: Chromium（デスクトップ・モバイル）
- **成果物**: 失敗時にテスト結果とビデオをアップロード

```yaml
# 手動でE2EテストのCIを実行
gh workflow run e2e.yml
```

## テスト対象

### 基本機能テスト (`basic-gameplay.spec.ts`)
- 初期状態の表示
- 駒の移動
- 手番の切り替え
- Undo/Redo機能
- ゲームリセット
- 移動履歴のナビゲーション

### レスポンシブデザインテスト (`responsive-design.spec.ts`)
- モバイル・タブレット・デスクトップレイアウト
- ビューポート変更時の動作継続
- タッチ操作
- 座標表示の適応

### アクセシビリティテスト (`accessibility.spec.ts`)
- ARIAラベル
- キーボードナビゲーション
- スクリーンリーダー対応
- フォーカス管理
- 色のコントラスト

## Page Objectパターン

`e2e/pages/shogi-board.page.ts` でPage Objectパターンを実装：

```typescript
// 使用例
const shogiBoardPage = new ShogiBoardPage(page);
await shogiBoardPage.goto();
await shogiBoardPage.makeMove(7, 7, 6, 7);
await shogiBoardPage.expectCurrentPlayer("後手番");
```

### 主要メソッド

- `makeMove(fromRow, fromCol, toRow, toCol)`: 駒を移動
- `expectCurrentPlayer(player)`: 手番の確認
- `expectMoveInHistory(notation)`: 棋譜の確認
- `resetGame()`: ゲームリセット
- `undo()` / `redo()`: 手の取り消し・やり直し

## Playwright MCP

Microsoft公式のPlaywright MCPを導入済み：

```bash
# MCPサーバーを起動（snapshot mode）
npm run test:mcp

# Vision modeで起動
npm run test:mcp:vision
```

### MCPの利点

1. **自然言語テスト**: 日本語でテストケースを記述可能
2. **アクセシビリティツリー**: 構造化されたページ解析
3. **LLM最適化**: AIによるテスト生成・デバッグ支援

## トラブルシューティング

### よくある問題

1. **要素が見つからない**
   ```bash
   # ページの状態を確認
   npx playwright test --debug
   ```

2. **レスポンシブレイアウトの問題**
   ```typescript
   // 複数要素の場合は.first()を使用
   await expect(page.locator("h2").filter({ hasText: "先手番" }).first()).toBeVisible();
   ```

3. **タイムアウト**
   ```typescript
   // タイムアウトを調整
   await expect(element).toBeVisible({ timeout: 10000 });
   ```

### デバッグ

```bash
# ヘッドレスモードを無効にして実行
npx playwright test --headed

# スロー実行
npx playwright test --headed --slowMo=1000

# 特定のテストをデバッグ
npx playwright test --debug basic-gameplay.spec.ts
```

## CI最適化

- **並列実行**: CIでは1ワーカーで安定性を優先
- **ブラウザ選択**: CI環境ではChromiumのみで実行時間短縮
- **リトライ**: CI環境では2回リトライ
- **アーティファクト**: 失敗時にレポートとビデオを保存

## 今後の拡張

1. **Visual Regression Testing**: スクリーンショット比較
2. **Performance Testing**: ページロード時間の測定
3. **API Testing**: バックエンドAPIとの統合テスト
4. **Cross-Browser Testing**: Firefox、Safariでの定期実行
