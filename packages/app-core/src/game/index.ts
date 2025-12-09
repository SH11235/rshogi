import type { PositionService } from "./position-service";
import { createTauriPositionService } from "./tauri-position-service";
import { createWasmPositionService } from "./wasm-position-service";

let cachedService: PositionService | null = null;

export const getPositionService = (): PositionService => {
    if (cachedService) {
        return cachedService;
    }

    const isTauri = typeof window !== "undefined" && "__TAURI__" in window;
    cachedService = isTauri ? createTauriPositionService() : createWasmPositionService();
    return cachedService;
};

export * from "./board";
export * from "./csa";
export * from "./position-service";
