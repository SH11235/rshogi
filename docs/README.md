# 将棋アプリケーション ドキュメント

このディレクトリには、将棋アプリケーションの技術文書が整理されています。

## 📁 ドキュメント構成

### 🏗️ architecture/ - アーキテクチャ設計
- [`state-management-patterns.md`](./architecture/state-management-patterns.md) - Zustandを使った状態管理パターン
- [`webrtc-patterns.md`](./architecture/webrtc-patterns.md) - WebRTC通信の実装パターン

### 🚀 features/ - 機能仕様
- [`ai-engine.md`](./features/ai-engine.md) - AIエンジンの詳細仕様
- [`opening-book.md`](./features/opening-book.md) - 定跡システムの統合仕様書
- [`shogi-app-enhanced-requirements.md`](./features/shogi-app-enhanced-requirements.md) - アプリケーション機能要件
- [`ai-benchmark.md`](./ai-benchmark.md) - AIのベンチマーク結果

### 🔧 development/ - 開発ガイド
- [`tdd-implementation-guide.md`](./development/tdd-implementation-guide.md) - TDD実装ガイド
- [`testing-strategies.md`](./development/testing-strategies.md) - テスト戦略とベストプラクティス
- [`online-play-implementation-plan.md`](./development/online-play-implementation-plan.md) - オンライン対戦の実装計画
- [`online-play-test-guide.md`](./development/online-play-test-guide.md) - オンライン対戦のテストガイド

### 📚 implementation/ - 実装ガイド
- [`opening-book-implementation-guide.md`](./implementation/opening-book-implementation-guide.md) - 定跡実装の詳細ガイド
- [`opening-book-tdd-implementation-guide.md`](./implementation/opening-book-tdd-implementation-guide.md) - 定跡のTDD実装ガイド

### 👤 user-guide/ - ユーザーガイド
- [`ai-guide.md`](./user-guide/ai-guide.md) - AIモードの使い方

### 🗄️ archive/ - アーカイブ
過去のドキュメントや古いバージョンの仕様書を保管しています。

## 🔗 関連ドキュメント

### プロジェクトルート
- [`/README.md`](../README.md) - プロジェクト概要
- [`/CLAUDE.md`](../CLAUDE.md) - Claude Code用の開発ガイドライン
- [`/GEMINI.md`](../GEMINI.md) - Gemini関連の設定

### パッケージ別
- [`/packages/web/README.md`](../packages/web/README.md) - Webフロントエンドの詳細
- [`/packages/rust-core/README.md`](../packages/rust-core/README.md) - Rustコアライブラリの詳細

## 📝 ドキュメント更新ガイドライン

1. **カテゴリの選択**: 新しいドキュメントは適切なカテゴリに配置してください
2. **命名規則**: ケバブケース（kebab-case）を使用し、内容が分かりやすい名前を付けてください
3. **更新時の注意**: 実装が変更された場合は、関連するドキュメントも必ず更新してください
4. **アーカイブ**: 古くなったドキュメントは削除せず、`archive/`ディレクトリに移動してください

## 🔍 クイックリンク

### 開発を始める
- [TDD実装ガイド](./development/tdd-implementation-guide.md)
- [テスト戦略](./development/testing-strategies.md)

### 機能を理解する
- [AIエンジン仕様](./features/ai-engine.md)
- [定跡システム](./features/opening-book.md)
- [オンライン対戦](./development/online-play-implementation-plan.md)

### アーキテクチャを学ぶ
- [状態管理パターン](./architecture/state-management-patterns.md)
- [WebRTCパターン](./architecture/webrtc-patterns.md)