import { setPositionServiceFactory } from "@shogi/app-core";
import { createWasmPositionService } from "./wasm-position-service";

/**
 * Web プラットフォーム用の PositionService を初期化
 * main.tsx の最初で呼び出す必要がある
 */
export const initializePositionService = (): void => {
    setPositionServiceFactory(() => createWasmPositionService());
};
