import type { EngineGoResult, EnginePort } from "../types";

const FALLBACK_MOVES = [
    "7g7f",
    "3c3d",
    "2g2f",
    "8c8d",
    "2b8h+",
    "resign",
];

export class WebEnginePort implements EnginePort {
    private started = false;
    private cursor = 0;
    private lastPosition = "startpos";

    async start(): Promise<void> {
        this.started = true;
        this.cursor = 0;
    }

    async setPosition(sfenOrMoves: string): Promise<void> {
        if (!this.started) {
            throw new Error("WebEnginePort must call start before setPosition");
        }

        this.lastPosition = sfenOrMoves;
    }

    async go(): Promise<EngineGoResult> {
        if (!this.started) {
            throw new Error("WebEnginePort must call start before go");
        }

        const move = FALLBACK_MOVES[this.cursor % FALLBACK_MOVES.length];
        this.cursor += 1;
        return {
            bestmove: move,
            pv: { moves: [move, "(stub)"] },
            score: { type: "cp", value: 0 },
        };
    }

    async stop(): Promise<void> {
        // noop for stub
    }

    async dispose(): Promise<void> {
        this.started = false;
        this.cursor = 0;
        this.lastPosition = "startpos";
    }

    get lastRequestedPosition(): string {
        return this.lastPosition;
    }
}
