# Coding Guidelines
- Prefer functional style over classes in TypeScript/JavaScript; use factory functions that close over state instead of `class`.
- Keep API signatures aligned with backend implementations; do not invoke non-existent IPC/commands.
- Use structured JSON for engine events (`info`/`bestmove`/`error`) instead of raw strings.

## Package roles (packages/*)
- `app-core`: ドメインロジック（局面/棋譜処理、エンジンポートなど）。UI依存なし。
- `design-system`: テーマ/トークン/Provider。shadcn/ui に依存する下地。
- `ui`: 共通 UI コンポーネント（デザインシステム前提）。必要になったものだけ昇格する。
- `engine-client`: EngineClient 型・インターフェースとモック。
- `engine-wasm`: Web/Wasm 実装（Worker 経由、wasm-bindgen 出力を隠蔽）。
- `engine-tauri`: Tauri IPC クライアント実装（invoke/listen）。実エンジン接続はここ経由。
- `rust-core`: Rust エンジン本体（engine-core/engine-usi 等）。

## 実装方針メモ
- Web と Desktop は極力足並みを揃え、同じ UI/ロジックを共有する。独自実装の分岐は最小限にする。

## UI-Specific Notes
- Desktop (Tauri) UI rules: see `apps/desktop/AGENTS.md` (StrictMode impact, engine client handling).
- Web (Wasm) UI rules: see `apps/web/AGENTS.md` (StrictMode impact, engine client handling).

## スタイリングルール

- 色はハードコード（`#ffffff`, `text-[#3a2a16]` 等）せず、デザインシステムの CSS 変数を使用する
  - 一般的な色: `bg-background`, `text-foreground`, `border-border` 等
  - 和風配色: `text-wafuu-sumi`, `bg-wafuu-shu`, `bg-wafuu-ai` 等
  - 将棋盤: `text-shogi-piece-text`, `bg-shogi-piece-bg`, `border-shogi-outer-border` 等
- 新しい色が必要な場合は `packages/design-system/src/theme.css` と `tailwind.preset.ts` に追加する

## テストファイルの配置

- テストファイルはソースファイルと同じディレクトリに `*.test.ts` または `*.test.tsx` として配置する
- `__tests__/` ディレクトリは使用しない
- 例: `hooks/useEngineManager.ts` → `hooks/useEngineManager.test.ts`

## Git操作に関する注意

**重要**: ユーザーの明示的な指示なしに、以下の操作を行ってはいけない:
- `git checkout` や `git restore` でファイルの変更を元に戻す
- `git reset` でコミットを取り消す
- その他、ユーザーの作業を勝手に変更・削除する操作

ユーザーは別セッションで並行作業している可能性があるため、ビルドエラーやテスト失敗が発生しても、勝手にコードをリセットせず、まずユーザーに確認すること。

ユーザーへの返答は日本語で行う事
