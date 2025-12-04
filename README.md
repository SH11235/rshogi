# 将棋ゲームエンジン（Shogi Game Engine）

Rust実装の将棋エンジンプロジェクトです。NNUE（Efficiently Updatable Neural Network）評価関数を搭載し、USIプロトコルに対応しています。

## 🚀 セットアップ

### 必要なツール

- **Rust**:
    ```bash
    $ rustup -V
    rustup 1.28.2 (e4f3ad6f8 2025-04-28)
    info: This is the version for the rustup toolchain manager, not the rustc compiler.
    info: The currently active `rustc` version is `rustc 1.91.1 (ed61e7d7e 2025-11-07)`
    ```
- **Node.js**: v24
- **pnpm**: パッケージマネージャー
- **wasm-bindgen-cli**: WASMビルド用（WebAssembly対応の場合）

### WASMビルドの準備

WebAssemblyビルドを実行する場合は、以下の設定が必要です：

```bash
# Rustのデフォルトツールチェーンを設定
rustup default stable

# WASMターゲットを追加
rustup target add wasm32-unknown-unknown

# wasm-bindgen-cliをインストール
cargo install wasm-bindgen-cli
```

## 📦 パッケージ構成

```
packages/
└── rust-core/              # 将棋AIエンジン（Rustワークスペース）
    ├── crates/
    │   ├── engine-core/    # コアエンジン実装（152ファイル）
    │   ├── engine-usi/     # USIプロトコルCLIインターフェース
    │   └── tools/          # NNUE訓練・解析ツール（60以上のバイナリ）
    ├── docs/               # 包括的なドキュメント（50以上のマークダウンファイル）
    └── Cargo.toml          # ワークスペース定義

apps/                       # 今後追加予定：GUIアプリケーション等
```

## 📄 ライセンス

MIT License
