import type { PositionService } from "./position-service";

let factory: (() => PositionService) | null = null;
let cachedService: PositionService | null = null;

export const setPositionServiceFactory = (create: () => PositionService): void => {
    factory = create;
    cachedService = null;
};

export const getPositionService = (): PositionService => {
    if (!factory) {
        throw new Error(
            "Position service factory is not initialized. Import @shogi/app-core entrypoint first.",
        );
    }
    if (!cachedService) {
        cachedService = factory();
    }
    return cachedService;
};
