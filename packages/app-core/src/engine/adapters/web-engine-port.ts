import type { EngineGoResult, EnginePort } from "../types";

const FALLBACK_MOVES = ["7g7f", "3c3d", "2g2f", "8c8d", "2b8h+", "resign"];

export function createWebEnginePort(): EnginePort & { lastRequestedPosition: string } {
    let started = false;
    let cursor = 0;
    let lastPosition = "startpos";

    return {
        async start(): Promise<void> {
            started = true;
            cursor = 0;
        },
        async setPosition(sfenOrMoves: string): Promise<void> {
            if (!started) {
                throw new Error("WebEnginePort must call start before setPosition");
            }

            lastPosition = sfenOrMoves;
        },
        async go(_params: {
            byoyomi?: number;
            btime?: number;
            wtime?: number;
        }): Promise<EngineGoResult> {
            if (!started) {
                throw new Error("WebEnginePort must call start before go");
            }

            const move = FALLBACK_MOVES[cursor % FALLBACK_MOVES.length];
            cursor += 1;
            return {
                bestmove: move,
                pv: { moves: [move, "(stub)"] },
                score: { type: "cp", value: 0 },
            };
        },
        async stop(): Promise<void> {
            // noop for stub
        },
        async dispose(): Promise<void> {
            started = false;
            cursor = 0;
            lastPosition = "startpos";
        },
        get lastRequestedPosition(): string {
            return lastPosition;
        },
    };
}
