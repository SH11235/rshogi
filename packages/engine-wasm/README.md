# @shogi/engine-wasm

WebAssembly版の将棋エンジンパッケージです。Rustで実装されたコアエンジンをWASMにコンパイルし、ブラウザ環境で動作させることができます。

## 必要なツール

WASMビルドには以下のセットアップが必要です（threaded ビルドは nightly 必須）：

```bash
# Rustのデフォルトツールチェーンを設定（single 用）
rustup default stable

# WASMターゲットを追加（single 用）
rustup target add wasm32-unknown-unknown

# threaded 用の nightly ツールチェーン
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly

# wasm-bindgen-cli を固定バージョンでインストール
cargo install wasm-bindgen-cli --version 0.2.106
```

## ビルド

```bash
pnpm --filter @shogi/engine-wasm build
```

内部的には以下のステップが実行されます：

1. `build:wasm`: RustコードをWASMにコンパイル（single + threaded の2系統）
2. `tsc`: TypeScriptのビルド

生成物：

- `packages/engine-wasm/pkg`: single-threaded 用 wasm
- `packages/engine-wasm/pkg-threaded`: threaded 用 wasm（`engine_wasm_worker.js` を含む）

WASM だけ再生成したい場合は次を実行します：

```bash
pnpm --filter @shogi/engine-wasm build:wasm
```

`build:wasm` は single + threaded を必ず両方作るため、nightly が未導入だと失敗します。

## threaded ビルドの詳細（試行錯誤メモ）

threaded ビルドは制約が多く、以下の前提で構成しています。

- **nightly 必須**: `-Z build-std=std,panic_abort` を使うため（wasm threads 用の std 再構築が必要）。
- **custom target spec**: `packages/rust-core/targets/wasm32-unknown-unknown.json` を使用して `+atomics,+bulk-memory,+mutable-globals` を有効化。
  - `-C target-feature=...` での指定は警告が出るため、target spec に寄せています。
- **shared memory/TLS export**: threaded ビルドは `--shared-memory` などの link-arg を追加（`build-wasm.mjs` 内）。
- **worker スクリプトの自前生成**: `wasm-bindgen --target web` は worker を出力しないため、
  `build-wasm.mjs` が `engine_wasm_worker.js` を生成します。
- **ThreadPool 初期化は JS 側**: `engine.worker.threaded.ts` で Wasm module/memory を保持し、
  `engine_wasm_worker.js` を複数起動して `{ module, memory, thread_stack_size }` を配布します
  （`DEFAULT_THREAD_STACK_SIZE=2MB`）。
- **出力検証**: `pkg-threaded/engine_wasm_worker.js` と `initThreadPool` export の存在をビルド時に検証しています。

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
