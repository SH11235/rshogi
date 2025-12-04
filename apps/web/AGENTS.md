# Web (Wasm) UI Guidelines

## Context
- Uses `@shogi/engine-wasm` (worker-based) for the engine.
- Debug entry points: `apps/web/src/main.tsx`, `apps/web/src/App.tsx`.
- Keep behavior aligned with the Tauri UI where practical, but wasm stays in-browser.

## React / StrictMode
- React 18+ StrictMode double-runs effects; avoid kicking off searches from mount effects.
- `useEffect` should handle `subscribe`/`unsubscribe` and cancel any active `SearchHandle` in cleanup.
- Trigger `engine.init/loadPosition/search` from explicit user actions (buttons/handlers) to avoid duplicate searches in StrictMode.
- Engine client should be a singleton (module-scope or stable ref); do not recreate per render.

## Engine client handling
- Subscribe in `useEffect`, unsubscribe in cleanup.
- Cancel active `SearchHandle` in cleanup; dispose the client only when intentionally tearing down the worker, not per render.
- Keep event payloads structured JSON (`info`/`bestmove`/`error`), field `move` for bestmove.
