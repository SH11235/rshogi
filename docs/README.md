# 将棋アプリケーション ドキュメント

このディレクトリには、将棋アプリケーションの技術文書が整理されています。

## 📁 ドキュメント構成

### 🏗️ architecture/ - アーキテクチャ設計
- [`state-management-patterns.md`](./architecture/state-management-patterns.md) - Zustandを使った状態管理パターン
- [`webrtc-patterns.md`](./architecture/webrtc-patterns.md) - WebRTC通信の実装パターン

### 🚀 features/ - 機能仕様
- [`ai-engine.md`](./features/ai-engine.md) - AIエンジンの詳細仕様

### 🔧 development/ - 開発ガイド
- [`tdd-implementation-guide.md`](./development/tdd-implementation-guide.md) - TDD実装ガイド
- [`testing-strategies.md`](./development/testing-strategies.md) - テスト戦略とベストプラクティス
- [`wasm-integration-plan.md`](./development/wasm-integration-plan.md) - WASM統合実装計画

### 📚 reference/ - 技術リファレンス
- **algorithms/** - 未実装アルゴリズムの仕様
  - [`iid-implementation-guide.md`](./reference/algorithms/iid-implementation-guide.md) - Internal Iterative Deepening
  - [`probcut-implementation-guide.md`](./reference/algorithms/probcut-implementation-guide.md) - ProbCut枝刈り
  - [`singular-extension-implementation-guide.md`](./reference/algorithms/singular-extension-implementation-guide.md) - Singular Extension
- **interfaces/** - インターフェース仕様
  - [`engine_interface_requirements_ja.md`](./reference/interfaces/engine_interface_requirements_ja.md) - エンジンインターフェース要件
- **requirements/** - システム要件
  - [`rust-shogi-ai-requirements.md`](./reference/requirements/rust-shogi-ai-requirements.md) - Rust AIエンジン要件
- [`wasm-api-quick-reference.md`](./reference/wasm-api-quick-reference.md) - WASM APIクイックリファレンス

### 🛠️ build/ - ビルド・デプロイ
- [`multi_target_build_strategy.md`](./build/multi_target_build_strategy.md) - マルチターゲットビルド戦略

### 🗄️ archive/ - アーカイブ
過去のドキュメントや実装済みの設計書を保管しています。

- **completed/** - 実装完了したドキュメント
  - `phase1-foundation-design.md` - Phase 1基盤設計（実装完了）
  - `phase2-nnue-design.md` - Phase 2 NNUE設計（実装完了）
  - `online-play-implementation-plan.md` - オンライン対戦実装計画（実装完了）
  - `online-play-test-guide.md` - オンライン対戦テストガイド（実装完了）
- **partial/** - 部分的に実装されたドキュメント
  - `phase3-search-enhancement-design.md` - Phase 3探索強化（部分実装）
- **abandoned/** - 中断または再計画予定のドキュメント
  - `phase4-integration-optimization-design.md` - Phase 4統合最適化（再計画予定）
  - `ai-rust-implementation-plan.md` - Rust AI実装計画（方針変更）
- **opening-book/** - 定跡関連のアーカイブ

## 🔗 関連ドキュメント

### プロジェクトルート
- [`/README.md`](../README.md) - プロジェクト概要
- [`/CLAUDE.md`](../CLAUDE.md) - Claude Code用の開発ガイドライン

### パッケージ別
- [`/packages/web/README.md`](../packages/web/README.md) - Webフロントエンドの詳細
- [`/packages/rust-core/README.md`](../packages/rust-core/README.md) - Rustコアライブラリの詳細

## 📝 ドキュメント更新ガイドライン

1. **カテゴリの選択**: 新しいドキュメントは適切なカテゴリに配置してください
2. **命名規則**: ケバブケース（kebab-case）を使用し、内容が分かりやすい名前を付けてください
3. **更新時の注意**: 実装が変更された場合は、関連するドキュメントも必ず更新してください
4. **アーカイブ**: 古くなったドキュメントは削除せず、適切な`archive/`サブディレクトリに移動してください

## 🔍 クイックリンク

### 開発を始める
- [TDD実装ガイド](./development/tdd-implementation-guide.md)
- [テスト戦略](./development/testing-strategies.md)

### 機能を理解する
- [AIエンジン仕様](./features/ai-engine.md)
- [未実装アルゴリズム](./reference/algorithms/)

### アーキテクチャを学ぶ
- [状態管理パターン](./architecture/state-management-patterns.md)
- [WebRTCパターン](./architecture/webrtc-patterns.md)