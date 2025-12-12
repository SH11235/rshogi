import type { PositionService } from "./position-service";
import {
    getPositionService as getRegisteredService,
    setPositionServiceFactory,
} from "./position-service-registry";
import { createWasmPositionService } from "./wasm-position-service";

setPositionServiceFactory(() => createWasmPositionService());

export const getPositionService = (): PositionService => getRegisteredService();

export * from "./board";
export * from "./csa";
export * from "./position-service";
