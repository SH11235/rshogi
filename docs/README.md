# Shogi Application Documentation

このディレクトリには、将棋アプリケーション全体の設計・実装ドキュメントが含まれています。

## ディレクトリ構造

### `/development/`
開発手法・ベストプラクティスに関するドキュメント
- [TDD Implementation Guide](./development/tdd-implementation-guide.md) - t-wada氏のTDDスタイルに基づく開発ガイド

### `/implementation/`
機能実装のためのガイドとプラン
- [Opening Book Implementation Guide](./implementation/opening-book-implementation-guide.md) - 定跡機能の実装ガイド（概要版）
- [Opening Book TDD Implementation Guide](./implementation/opening-book-tdd-implementation-guide.md) - 定跡機能のTDD実装ガイド（詳細版）
- [Online Play Implementation Plan](./online-play-implementation-plan.md) - オンライン対戦機能の実装計画

### `/features/`
各機能の仕様・設計ドキュメント
- [AI Engine](./features/ai-engine.md) - AIエンジンの設計と実装
- [Enhanced Requirements](./features/shogi-app-enhanced-requirements.md) - アプリケーションの拡張要件

### `/user-guide/`
ユーザー向けガイド
- [AI Guide](./user-guide/ai-guide.md) - AI機能の使い方

### `/summaries/`
実装完了機能のまとめ
- [Opening Book Implementation Summary](./opening-book-implementation-summary.md) - 定跡機能実装のまとめ

### その他のドキュメント
- [AI Benchmark](./ai-benchmark.md) - AIパフォーマンスのベンチマーク結果
- [State Management Patterns](./state-management-patterns.md) - 状態管理のパターン
- [Testing Strategies](./testing-strategies.md) - テスト戦略
- [WebRTC Patterns](./webrtc-patterns.md) - WebRTC実装パターン
- [Online Play Test Guide](./online-play-test-guide.md) - オンライン対戦のテストガイド

## パッケージ固有のドキュメント

### Rust Core (`packages/rust-core/docs/`)
- `/tools/` - CLIツールのドキュメント
- `/reference/` - 技術仕様・フォーマット定義
- `/development/` - Rust固有の開発ガイド

### Web (`packages/web/`)
- `/.claude/` - Claude Code用の参照ドキュメント
- `/src/` - コンポーネント・サービスのドキュメント

## ドキュメント作成ガイドライン

1. **配置場所の決定**
   - モノレポ全体に関わるドキュメント → ルートの`/docs`
   - パッケージ固有のドキュメント → 各パッケージの`docs`ディレクトリ

2. **命名規則**
   - ケバブケース（kebab-case）を使用
   - 機能名を明確に含める
   - 実装ガイドは`-implementation-guide.md`で終わる

3. **内容の構成**
   - 概要と前提条件を明記
   - ステップバイステップの手順
   - コード例を豊富に含める
   - トラブルシューティングセクション

4. **更新頻度**
   - 実装と同時にドキュメントを更新
   - 破壊的変更時は必ず更新
   - レビュー時にドキュメントも確認