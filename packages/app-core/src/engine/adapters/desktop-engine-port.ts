import { invoke } from "@tauri-apps/api/core";
import type { EngineGoResult, EnginePort, Pv, Score } from "../types";

interface HandshakeResponse {
    name?: string | null;
    author?: string | null;
    options?: Array<{ name: string; raw: string }>;
}

interface BestMoveDto {
    bestmove: string;
    ponder?: string | null;
    info?: InfoDto | null;
}

interface InfoDto {
    depth?: number | null;
    nodes?: number | null;
    nps?: number | null;
    score?: { type: string; value: number } | null;
    pv?: string[] | null;
    raw: string;
}

export class DesktopEnginePort implements EnginePort {
    private handshake?: HandshakeResponse;
    private enginePath?: string;

    async start(opts?: { enginePath?: string }): Promise<void> {
        const resolvedPath = opts?.enginePath ?? this.enginePath;
        if (!resolvedPath) {
            throw new Error("DesktopEnginePort.start requires an enginePath");
        }

        this.enginePath = resolvedPath;
        this.handshake = await invoke<HandshakeResponse>("engine_start", {
            enginePath: resolvedPath,
        });
    }

    async setPosition(sfenOrMoves: string): Promise<void> {
        await invoke("engine_position", { payload: sfenOrMoves });
    }

    async go(params: {
        byoyomi?: number;
        btime?: number;
        wtime?: number;
    }): Promise<EngineGoResult> {
        const dto = await invoke<BestMoveDto>("engine_go", { params });
        return {
            bestmove: dto.bestmove,
            pv: dto.info?.pv ? ({ moves: dto.info.pv } satisfies Pv) : undefined,
            score: dto.info?.score
                ? convertScore(dto.info.score.type, dto.info.score.value)
                : undefined,
        };
    }

    async stop(): Promise<void> {
        await invoke("engine_stop");
    }

    async dispose(): Promise<void> {
        await this.stop().catch(() => undefined);
    }

    get metadata(): HandshakeResponse | undefined {
        return this.handshake;
    }
}

function convertScore(type: string, value: number): Score | undefined {
    if (type === "cp") {
        return { type: "cp", value };
    }
    if (type === "mate") {
        return { type: "mate", value };
    }

    return undefined;
}
