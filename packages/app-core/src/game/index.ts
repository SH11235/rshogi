import type { PositionService } from "./position-service";
import { createTauriPositionService } from "./tauri-position-service";
import { createWasmPositionService } from "./wasm-position-service";

let cachedService: PositionService | null = null;

export const getPositionService = (): PositionService => {
    if (cachedService) {
        return cachedService;
    }

    const isTauri =
        typeof window !== "undefined" &&
        (("__TAURI__" in window) ||
            ("__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>)) ||
            typeof (window as { __TAURI_IPC__?: unknown }).__TAURI_IPC__ === "function");
    cachedService = isTauri ? createTauriPositionService() : createWasmPositionService();
    return cachedService;
};

export * from "./board";
export * from "./csa";
export * from "./position-service";
