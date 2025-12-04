# Coding Guidelines
- Prefer functional style over classes in TypeScript/JavaScript; use factory functions that close over state instead of `class`.
- Keep API signatures aligned with backend implementations; do not invoke non-existent IPC/commands.
- Use structured JSON for engine events (`info`/`bestmove`/`error`) instead of raw strings.

## UI-Specific Notes
- Desktop (Tauri) UI rules: see `apps/desktop/AGENTS.md` (StrictMode impact, engine client handling).
- Web (Wasm) UI rules: see `apps/web/AGENTS.md` (StrictMode impact, engine client handling).

ユーザーへの返答は日本語で行う事
