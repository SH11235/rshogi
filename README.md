# 将棋ゲームエンジン（Shogi Game Engine）

TypeScript で実装された本格的な将棋ゲームエンジン。Web、Discord Bot、デスクトップアプリなど複数プラットフォームに対応したモノレポ構成。

## 🎮 今すぐプレイ

**🌐 GitHub Pages:** [https://sh11235.github.io/shogi/](https://sh11235.github.io/shogi/)

完全な将棋ルールに対応した Web アプリで、王手放置の禁止、詰み判定、成り、持ち駒の打ちなど、すべての機能が実装されています。

## 📦 パッケージ構成

```
packages/
├── core/          # 🎯 共有将棋エンジン（ドメインロジック）
├── types/         # 🔧 共有型定義
├── web/           # 🌐 Webクライアント（React + Vite）
├── rust-core/     # 🦀 Rust/WebAssembly P2P通信
├── server/        # ⚡ ゲームサーバー（Express + WebSocket）
├── discord-bot/   # 🤖 Discord Bot
└── desktop/       # 🖥️ デスクトップアプリ（Tauri）
```

## 🚀 クイックスタート

### 前提条件

- **Node.js**: `v24.x`
- **npm**: `v11.x`
- 上記バージョンは [Volta](https://docs.volta.sh/guide/getting-started) で管理
- **Rust** (P2P機能を使用する場合): [rustup](https://rustup.rs/) でインストール
- **wasm-pack** (P2P機能を使用する場合): `cargo install wasm-pack`

### インストール

```bash
# 依存関係をインストール
npm install

# 全パッケージをビルド
npm run build
```

⚠️ **重要**: 初回インストール時、P2P機能用のWASMファイルが自動的にビルドされます。
もしエラーが発生した場合は、以下を実行してください：

```bash
# rust-coreディレクトリでWASMをビルド
cd packages/rust-core
npm run build:wasm
```

### 開発

```bash
# 全パッケージの開発サーバーを起動
npm run dev

# 特定のパッケージのみ開発
npm run dev --workspace=@shogi/web       # Webクライアント
npm run dev --workspace=@shogi/server    # サーバー
npm run dev --workspace=@shogi/discord-bot # Discord Bot
```

## 🧪 テスト・品質管理

```bash
# 全パッケージのテスト実行
npm test

# 変更された部分のみテスト（高速）
npm run test:affected

# Lint + フォーマット
npm run lint
npm run format

# 型チェック
npm run typecheck
```

## 📋 利用可能なコマンド

### ルートレベルコマンド

| コマンド | 説明 |
|---------|------|
| `npm run build` | 全パッケージをビルド |
| `npm run dev` | 全パッケージの開発サーバー起動 |
| `npm test` | 全パッケージのテスト実行 |
| `npm run lint` | 全パッケージのLint実行 |
| `npm run typecheck` | 全パッケージの型チェック |
| `npm run clean` | ビルド成果物とnode_modulesを削除 |

### 効率的なコマンド（Turbo活用）

| コマンド | 説明 |
|---------|------|
| `npm run test:affected` | 変更されたパッケージのみテスト |
| `npm run lint:affected` | 変更されたパッケージのみLint |
| `npm run typecheck:affected` | 変更されたパッケージのみ型チェック |

### パッケージ個別操作

```bash
# Webクライアント
npm run dev --workspace=@shogi/web
npm run build --workspace=@shogi/web
npm test --workspace=@shogi/web

# 将棋エンジンコア
npm test --workspace=shogi-core
npm run build --workspace=shogi-core

# サーバー
npm run dev --workspace=@shogi/server
npm run start --workspace=@shogi/server
```

### MCP

- human-in-the-loop
https://github.com/KOBA789/human-in-the-loop

## 🏗️ アーキテクチャ

### 将棋エンジンコア (`packages/core/`)

- **純粋関数**: React/UI層に依存しない
- **完全なルール実装**: 二歩、打ち歩詰め、行き所のない駒など
- **国際化対応**: 英語キー + 日本語表示名
- **JSON安全**: ネットワーク通信・保存に対応

### Rust/WebAssembly P2P通信 (`packages/rust-core/`)

- **WebRTC P2P通信**: matchbox_socketを使用
- **WASM統合**: wasm-bindgenでJavaScriptとの相互運用
- **ビルド成果物**: `packages/web/src/wasm/`に自動配置
- **開発注意**: Rustコード変更時は`npm run build:wasm`の実行が必要

### テスト済み機能

- ✅ 264テスト全通過（Core: 75 + Web: 189）
- ✅ 基本的な駒の動き
- ✅ 成り処理
- ✅ 王手放置の禁止
- ✅ 特殊ルール（二歩、打ち歩詰め等）
- ✅ 詰み判定
- ✅ 合法手生成
- ✅ 千日手・連続王手の千日手
- ✅ 持将棋（入玉）判定
- ✅ トライルール

### プラットフォーム対応

| プラットフォーム | 状態 | 技術スタック |
|-------------|-----|------------|
| **Web** | ✅ 実装済み | React + Vite + Zustand |
| **サーバー** | 🚧 準備中 | Express + WebSocket |
| **Discord Bot** | 🚧 準備中 | discord.js |
| **デスクトップ** | 🚧 準備中 | Tauri + React |

## 🛠️ 開発ツール

- **ビルドシステム**: [Turbo](https://turbo.build/) - モノレポ最適化
- **Lint/Format**: [Biome](https://biomejs.dev/) - 高速な一体型ツール
- **型チェック**: TypeScript strict mode
- **テスト**: [Vitest](https://vitest.dev/) - 高速テストランナー
- **Git Hooks**: Husky + lint-staged

## 📖 詳細ドキュメント

- [将棋ルール実装詳細](./CLAUDE.md)
- [各パッケージのREADME](./packages/*/README.md)
- [コントリビューションガイド](./CONTRIBUTING.md)

## 🤖 AI対戦機能

### 難易度設定

| 難易度 | 探索深度 | 思考時間 | 特徴 |
|---------|----------|----------|------|
| **初心者** | 2手 | 1秒 | ランダム要素あり、学習に最適 |
| **中級者** | 4手 | 3秒 | 基本的な戦術を理解 |
| **上級者** | 6手 | 5秒 | しっかりとした守備と攻撃 |
| **エキスパート** | 8手 | 10秒 | 最強設定、挑戦的 |

### AIエンジンの特徴

#### 探索アルゴリズム
- **反復深化探索**: 時間を有効活用し、段階的に探索を深める
- **αβ枝刈り**: 不要な探索を省略し効率化
- **置換表（Transposition Table）**: 同一局面の再計算を回避
- **ムーブオーダリング**: 良い手を優先的に探索
  - PV（主要変化）の手を最優先
  - キラームーブ（過去に良かった手）
  - MVV-LVA（価値の高い駒を取る手を優先）
  - 成る手、王手を優先

#### 評価関数
- **駒の価値評価**: 歩=100点、飛車=1040点など伝統的な評価値
- **位置評価**: 駒種ごとの詳細な位置評価テーブル
- **王の安全性**: 守り駒の配置、王手状態、敵駒からの攻撃を考慮
- **駒の活動性**: 各駒の可動範囲（利きの数）を評価
- **駒の連携**: 複数の駒が協調している状態を高評価
- **成り駒ボーナス**: と金=520点など成り駒の価値を適切に評価

### AIパフォーマンスベンチマーク

AIエンジンの性能を測定するベンチマークツールが利用可能です：

```bash
# packages/core ディレクトリで実行
cd packages/core
npm run benchmark:ai
```

ベンチマークは各難易度のAIに対して、初期局面・中盤・終盤の3つの局面で測定を行い、以下の指標を出力します：
- 平均思考時間
- 探索深度
- 探索ノード数
- 定跡使用率

詳細な結果は `benchmark-results.md` ファイルに保存されます。

### 使い方

1. Webアプリで「AI対戦」を選択
2. 難易度と手番（先手/後手）を選択
3. 「対局開始」をクリック
4. AIの思考中は「思考中...」表示
5. 評価値、探索深度、ノード数などの情報をリアルタイム表示

## 🔍 詰み探索機能

- **最大7手詰めまで探索**: 高速な深さ優先探索
- **詳細な手順表示**: 駒種、移動元・先を明記
- **解析モードで利用可能**: 対局中は表示されません
- **パフォーマンス情報**: 探索ノード数、実行時間を表示

## ⏱️ 時間管理システム

### 対応している時間制

- **基本時間制**: 持ち時間 + 秒読み
- **フィッシャールール**: 1手ごとに時間加算
- **1手ごと制限**: 各手に制限時間
- **考慮時間制**: プロ対局で使用される方式

### 考慮時間制の特徴

- 持ち時間が残り少なくなったときに使用
- 1分単位で消費、回数制限あり
- プロ棋士の対局と同じルールを再現

## 🎮 将棋ルール対応状況

### ✅ 実装済み

- 基本的な駒の動き（歩、香、桂、銀、金、角、飛、王・玉）
- 成り処理（強制成りを含む）
- **王手放置の禁止** - 自分の王が王手される手は指せません
- 王手・詰み判定
- 持ち駒からの打ち手
- 二歩チェック
- 打ち歩詰めチェック
- 行き所のない駒チェック
- **千日手判定** - 同一局面が4回出現で引き分け
- **連続王手の千日手** - 連続王手による千日手は王手側の負け
- **持将棋（入玉）判定** - 24点法による入玉宣言勝ち
- **トライルール** - 自玉が相手玉の初期位置に到達で勝利
- 視覚的フィードバック（ハイライト、選択状態）
- ゲーム状態表示（手番、手数、勝敗）
- **AI対戦機能** - 4段階の難易度（初心者/中級者/上級者/エキスパート）
- **詰み探索機能** - 最大7手詰めまでの詰み手順を探索・表示
- **考慮時間制** - プロ対局で使用される時間管理システム

### 🚧 今後実装予定

- マルチプレイヤー対応（サーバー）
- Discord Bot機能
- デスクトップアプリ（Tauri）
- 棋譜データベース・検索機能
- 定跡データベース
- AI強化（機械学習による評価関数、並列探索）
- 観戦モード（リアルタイム観戦機能）
- 検討モード（局面解析、候補手表示）

## 📄 ライセンス

MIT License

---

**多プラットフォーム対応の将棋ゲーム開発プロジェクト**
