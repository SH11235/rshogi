# 将棋アプリ - 次期開発タスクリスト

## 🎯 開発状況

### 棋譜
  主要なフォーマット：
  1. KIF形式（柿木形式）
    - 最も一般的で広く使われている
    - 日本語表記（例：７六歩(77)）
    - ヘッダー情報（開始日時、終了日時、手合割、対局者名など）
    - コメント機能（#で始まる行）
    - 変化手順の記録も可能
  2. CSA形式
    - コンピュータ将棋協会標準
    - プログラム間のデータ交換を目的として設計
    - より構造化されたフォーマット
  3. SFEN形式
    - USIプロトコルベース
    - エンジン連携に適している
    - 局面表現にも使用
  4. KI2形式
    - KIF形式の変種
    - 移動元の座標を記録しない（例：５八金右）
● 将棋棋譜 Import/Export 機能要件定義

  5. 技術的要件

  パフォーマンス

  - 大きな棋譜ファイル（1000手以上）の処理
  - ストリーミング処理による省メモリ化
  - Web Worker活用（重い処理の非同期化）

  セキュリティ

  - アップロードファイルのサイズ制限
  - ファイル形式の検証
  - XSS対策（ユーザー入力のサニタイズ）

  ストレージ

  - IndexedDBでの棋譜保存
  - クラウド同期オプション
  - エクスポート設定の永続化

  6. 拡張性考慮事項

  プラグインアーキテクチャ

  - 新フォーマット追加の容易性
  - カスタムパーサー/フォーマッターの登録

  API設計

  interface KifuImporter {
    parse(input: string | File): Promise<Game>
    validate(input: string | File): Promise<ValidationResult>
    detectFormat(input: string | File): Format
  }

  interface KifuExporter {
    format(game: Game, options: ExportOptions): string
    download(game: Game, filename: string, options: ExportOptions): void
  }


## 🟢 低優先度（高度な機能）

### 8. **局面解析ツール**
- **実装内容**:
  - 現在の形勢判断（優勢/劣勢表示）
  - 次の一手候補の表示
  - 駒の価値計算
  - 攻防バランスの可視化
- **期待効果**: 学習支援、戦術理解の深化

### 11. **AIプレイヤー**
- **実装内容**:
  - 複数の強さレベル（初級～上級）
  - minimax算法による思考エンジン
  - 定跡データベースの実装
  - AI思考中の進捗表示
- **期待効果**: 一人用モードの提供、練習相手

## 📋 実装時の注意事項

### 技術的要件
- **テスト**: 新機能には必ずユニットテスト + E2Eテストを追加
- **型安全性**: TypeScript strict modeを維持
- **アクセシビリティ**: WCAG 2.1 AA準拠
- **パフォーマンス**: 60fps維持、メモリリーク防止

### 品質チェック
実装後は必ず以下を実行：
```bash
npm run build      # ビルド確認
npm run typecheck  # 型チェック
npm run lint       # コード品質
npm run format:check # フォーマット
npm run test       # ユニットテスト
npm run test:e2e   # E2Eテスト
```
