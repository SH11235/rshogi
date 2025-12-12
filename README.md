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

### 開発環境のセットアップ

#### Windows環境での重要な設定

Windows環境で開発する場合、改行コードの自動変換を無効にする必要があります：

```bash
git config core.autocrlf false
```

**理由**：
- 本プロジェクトでは全てのテキストファイルでLF改行を使用しています（`.gitattributes`で設定済み）
- `core.autocrlf=true`の場合、`cargo fmt`実行時に改行コードの変換により、ファイル全体が変更されたように見える問題が発生します
- 特にpre-commitフックでの自動フォーマット時に予期しない変更が発生する可能性があります

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
