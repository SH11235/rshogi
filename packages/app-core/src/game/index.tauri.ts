import type { PositionService } from "./position-service";
import {
    getPositionService as getRegisteredService,
    setPositionServiceFactory,
} from "./position-service-registry";
import { createTauriPositionService } from "./tauri-position-service";

setPositionServiceFactory(() => createTauriPositionService());

export const getPositionService = (): PositionService => getRegisteredService();

export * from "./board";
export * from "./csa";
export * from "./position-service";
