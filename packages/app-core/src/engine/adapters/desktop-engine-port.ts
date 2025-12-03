import { invoke } from "@tauri-apps/api/core";
import type { EngineGoResult, EnginePort } from "../types";

interface HandshakeResponse {
    name?: string | null;
    author?: string | null;
    options?: Array<{ name: string; raw: string }>;
}

/**
 * Functional style EnginePort for Desktop (Tauri IPC).
 * Backend emits events; go() just triggers search.
 */
export function createDesktopEnginePort(): EnginePort & { metadata?: HandshakeResponse } {
    let handshake: HandshakeResponse | undefined;

    return {
        async start(): Promise<void> {
            await invoke("engine_init", { opts: null });
            handshake = { name: "tauri-engine-mock" };
        },
        async setPosition(sfenOrMoves: string): Promise<void> {
            const [sfenPart, movesPart] = sfenOrMoves.split(" moves ");
            const sfen = sfenPart ?? "startpos";
            const moves = movesPart?.trim()?.length
                ? movesPart.trim().split(/\s+/).filter(Boolean)
                : undefined;
            await invoke("engine_position", { sfen, moves });
        },
        async go(params: {
            byoyomi?: number;
            btime?: number;
            wtime?: number;
        }): Promise<EngineGoResult> {
            await invoke("engine_search", { params });
            return { bestmove: "resign" }; // real result arrives via events
        },
        async stop(): Promise<void> {
            await invoke("engine_stop");
        },
        async dispose(): Promise<void> {
            await invoke("engine_stop").catch(() => undefined);
        },
        get metadata() {
            return handshake;
        },
    };
}
