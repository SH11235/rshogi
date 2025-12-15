import { setPositionServiceFactory } from "@shogi/app-core";
import { createTauriPositionService } from "./tauri-position-service";

/**
 * Desktop プラットフォーム用の PositionService を初期化
 * main.tsx の最初で呼び出す必要がある
 */
export const initializePositionService = (): void => {
    setPositionServiceFactory(() => createTauriPositionService());
};
