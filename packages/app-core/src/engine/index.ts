import { createDesktopEnginePort } from "./adapters/desktop-engine-port";
import { createWebEnginePort } from "./adapters/web-engine-port";
import type { EnginePort } from "./types";

export { createDesktopEnginePort, createWebEnginePort };
export * from "./types";

export type EnginePortKind = "desktop" | "web";

export function createEnginePort(kind: EnginePortKind): EnginePort {
    if (kind === "desktop") {
        return createDesktopEnginePort();
    }

    return createWebEnginePort();
}
