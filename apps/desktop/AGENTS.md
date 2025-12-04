# Desktop (Tauri) UI Guidelines

## Context
- Uses the native engine via `@shogi/engine-tauri`.
- Debug entry points: `apps/desktop/src/main.tsx`, `apps/desktop/src/App.tsx`.
- Engine work (init/loadPosition/search) is heavy/native; avoid triggering twice.

## React / StrictMode
- React 18+ StrictMode double-runs effects. Do **not** start searches in mount effects; prefer explicit user actions (buttons/handlers).
- `useEffect` should only manage `subscribe`/`unsubscribe` and in-flight `SearchHandle` cancellation.
- Engine client must be a singleton (module-scope or top-level ref); never recreate per render.
- If you must auto-run a search on mount for debugging, you may temporarily drop `<React.StrictMode>` in `src/main.tsx` with a short code comment explaining why (duplicate native searches otherwise).

## Engine client handling
- Always `subscribe` in `useEffect` and `unsubscribe` in cleanup.
- Cancel any active `SearchHandle` in cleanup; do not dispose the client per render.
- Keep event payloads structured JSON (`info`/`bestmove`/`error`), field `move` for bestmove.
