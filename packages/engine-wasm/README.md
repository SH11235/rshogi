# @shogi/engine-wasm

WebAssembly版の将棋エンジンパッケージです。Rustで実装されたコアエンジンをWASMにコンパイルし、ブラウザ環境で動作させることができます。

## 必要なツール

WASMビルドには以下のセットアップが必要です：

```bash
# Rustのデフォルトツールチェーンを設定
rustup default stable

# WASMターゲットを追加
rustup target add wasm32-unknown-unknown

# wasm-bindgen-cliをインストール
cargo install wasm-bindgen-cli
```

## ビルド

```bash
pnpm build
```

内部的には以下のステップが実行されます：

1. `build:wasm`: RustコードをWASMにコンパイル
2. `tsc`: TypeScriptのビルド

## 使用方法

```typescript
import { ShogiEngine } from '@shogi/engine-wasm';

const engine = new ShogiEngine();
// エンジンの使用
```
