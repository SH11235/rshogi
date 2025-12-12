# WASM ビルドとターゲット分離の方針

## 目的
- web でのみ `@shogi/engine-wasm` を利用し、desktop (Tauri) には依存グラフごと含めない。
- エントリポイントをターゲット別に固定し、deep import を禁止して初期化漏れや tree-shaking 事故を防ぐ。

## 構成
- `@shogi/app-core` の公開入口はトップレベルのみ。`@shogi/app-core/game` 等の deep import は禁止。
- エントリ:
  - web: `packages/app-core/src/index.web.ts`（wasm-position-service を登録）
  - desktop: `packages/app-core/src/index.tauri.ts`（tauri-position-service を登録）
- Vite/TS の alias/paths:
  - desktop: `@shogi/app-core` を `index.tauri.ts` に向ける（engine-wasm は paths から除外）。
  - web: `@shogi/app-core` を `index.web.ts` に向ける（engine-tauri の alias/paths は削除）。
- sideEffects:
  - `packages/app-core/package.json` で factory 登録がある出力ファイルを明示列挙し、ツリーシェイクで副作用が落ちるのを防止。

## ビルド/検証のメモ
- web で wasm を更新する場合: `pnpm --filter @shogi/engine-wasm build:wasm` を実行し、`packages/engine-wasm/pkg` を再生成する。
- desktop の成果物確認（推奨チェック）:
  - `apps/desktop/dist` に `.wasm` / `engine.worker-*.js` が無いこと。
  - バンドル内に `@shogi/engine-wasm` 由来の参照が無いことを簡易 grep で確認。

## 補足
- TypeScript の `paths` は base/各 app でファイル指しに統一しており、desktop 側の `@shogi/engine-wasm` 解決は型レベルでも不可。
- wasm の初期化は `ensureWasmModule` による一度きりの Promise で行い、各 API 呼び出し前に await して未初期化パニックを防止。
