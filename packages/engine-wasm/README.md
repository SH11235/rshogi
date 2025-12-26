# @shogi/engine-wasm

WebAssembly版の将棋エンジンパッケージです。Rustで実装されたコアエンジンをWASMにコンパイルし、ブラウザ環境で動作させることができます。

## 必要なツール

WASMビルドには以下のセットアップが必要です（threaded ビルドは nightly 必須）：

```bash
# Rustのデフォルトツールチェーンを設定（single 用）
rustup default stable

# WASMターゲットを追加（single 用）
rustup target add wasm32-unknown-unknown

# threaded 用の nightly ツールチェーン（固定版）
rustup toolchain install nightly-2025-12-25
rustup component add rust-src --toolchain nightly-2025-12-25

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

`build:wasm` は single + threaded を必ず両方作るため、固定版 nightly が未導入だと失敗します。
別バージョンを使う場合は `RUST_NIGHTLY_TOOLCHAIN` で上書きできます（例: `nightly-YYYY-MM-DD`）。

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

## ブラウザ対応状況

| ブラウザ        | single | threaded | 備考                                      |
| --------------- | ------ | -------- | ----------------------------------------- |
| Chrome 92+      | ✅      | ✅        | 完全対応                                  |
| Edge 92+        | ✅      | ✅        | 完全対応                                  |
| Firefox 89+     | ✅      | ✅        | 完全対応                                  |
| Safari 15.2+    | ✅      | ⚠️        | SharedArrayBuffer 制限あり（COOP/COEP 必須） |

**threaded ビルドの追加要件**:
- `crossOriginIsolated === true` であること
- `SharedArrayBuffer` が利用可能であること
- サーバーが以下のヘッダーを返すこと:
  - `Cross-Origin-Opener-Policy: same-origin`
  - `Cross-Origin-Embedder-Policy: require-corp`

## トラブルシューティング

### threaded ビルドが動作しない

1. **`crossOriginIsolated` が `false`**: COOP/COEP ヘッダーを確認してください
2. **`SharedArrayBuffer` が `undefined`**: ブラウザのバージョンを確認してください
3. **メモリエラー**: スレッド数を減らして再試行してください（推奨: CPUコア数の50-75%）

### ビルドエラー

1. **nightly ツールチェーンが見つからない**: `rustup toolchain install nightly-2025-12-25` を実行
2. **wasm-bindgen バージョン不一致**: `cargo install wasm-bindgen-cli --version 0.2.106` を実行
3. **rust-src が見つからない**: `rustup component add rust-src --toolchain nightly-2025-12-25` を実行

## パフォーマンスチューニング

- **推奨スレッド数**: CPU コア数の 50-75%（例: 8コアなら 4スレッド）
- **メモリ使用量**: ベース + (threads - 1) × 2MB + TT サイズ
- **最大スレッド数**: 4（安定性のため上限を設定）

## 使用方法

```typescript
import { createWasmEngineClient } from '@shogi/engine-wasm';

const engine = createWasmEngineClient();
await engine.init({ threads: 4 });
// エンジンの使用
```
