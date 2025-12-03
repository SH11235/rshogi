import type { EnginePort } from "./types";
import { createDesktopEnginePort } from "./adapters/desktop-engine-port";
import { createWebEnginePort } from "./adapters/web-engine-port";

export * from "./types";
export { DesktopEnginePort } from "./adapters/desktop-engine-port";
export { WebEnginePort } from "./adapters/web-engine-port";

export type EnginePortKind = "desktop" | "web";

export function createEnginePort(kind: EnginePortKind): EnginePort {
    if (kind === "desktop") {
        return createDesktopEnginePort();
    }

    return createWebEnginePort();
}
