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

## ベンチマーク

WASMビルド済みの状態で、NNUEモデルを指定してベンチを実行できます。

```bash
pnpm --filter @shogi/engine-wasm build:wasm
pnpm --filter @shogi/engine-wasm bench:wasm -- --nnue-file /path/to/nn.bin > wasm_bench.json
```

デフォルトはYaneuraOu準拠の4局面を使用します。任意の局面を使う場合は
`--sfens` で SFEN リストファイルを指定してください。

Material評価のみを計測する場合は `--material` を指定します。

## 使用方法

```typescript
import { ShogiEngine } from '@shogi/engine-wasm';

const engine = new ShogiEngine();
// エンジンの使用
```
