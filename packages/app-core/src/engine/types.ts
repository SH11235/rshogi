export type Score = { type: "cp"; value: number } | { type: "mate"; value: number };

export type Pv = {
    moves: string[];
};

export interface EngineGoResult {
    bestmove: string;
    pv?: Pv;
    score?: Score;
}

export interface EnginePort {
    start(opts?: { enginePath?: string }): Promise<void>;
    setPosition(sfenOrMoves: string): Promise<void>;
    go(params: { byoyomi?: number; btime?: number; wtime?: number }): Promise<EngineGoResult>;
    stop(): Promise<void>;
    dispose(): Promise<void>;
}
