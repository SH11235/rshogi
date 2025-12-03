# Desktop App (Tauri + React)

## 概要
- 将棋エンジンを Tauri 経由で呼び出すデスクトップアプリ（現状はモックエンジンをイベントで返す状態）。

## セットアップ
- ルートで `pnpm install` を実行。
- `pnpm tauri dev` で開発サーバー起動。

## ビルド
- `pnpm tauri build`

## アーキテクチャ
- フロントは `@shogi/engine-tauri` 経由で `engine_init/engine_search/engine_position/engine_stop` を呼び、`engine://event` で `info/bestmove/error` を受信。
- バックエンドは `apps/desktop/src-tauri/src/lib.rs` に実装（今はモック、後で実エンジンに差し替え）。

## 既知の制限
- エンジンはモック応答のみ。実エンジン接続は今後のタスク。

This template should help get you started developing with Tauri, React and Typescript in Vite.

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
