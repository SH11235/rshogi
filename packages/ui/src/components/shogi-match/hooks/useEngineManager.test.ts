import type { PositionState } from "@shogi/app-core";
import type { EngineEvent } from "@shogi/engine-client";
import { act, renderHook } from "@testing-library/react";
import type { MutableRefObject } from "react";
import { describe, expect, it, vi } from "vitest";
import { formatEvent, useEngineManager } from "./useEngineManager";

const applyMoveWithStateMock = vi.fn();

vi.mock("@shogi/app-core", async () => {
    const actual = await vi.importActual<typeof import("@shogi/app-core")>("@shogi/app-core");
    return {
        ...actual,
        applyMoveWithState: (...args: Parameters<typeof actual.applyMoveWithState>) =>
            applyMoveWithStateMock(...args),
    };
});

describe("formatEvent", () => {
    it("bestmove イベントを正しくフォーマットする", () => {
        const event: EngineEvent = {
            type: "bestmove",
            move: "7g7f",
        };
        const result = formatEvent(event, "S:engine1");
        expect(result).toBe("[S:engine1] bestmove 7g7f");
    });

    it("info イベントを正しくフォーマットする（全フィールド）", () => {
        const event: EngineEvent = {
            type: "info",
            depth: 10,
            seldepth: 15,
            scoreCp: 150,
            nodes: 100000,
            nps: 50000,
            pv: ["7g7f", "3c3d", "2g2f"],
        };
        const result = formatEvent(event, "G:engine2");
        expect(result).toBe(
            "[G:engine2] info depth 10 seldepth 15 score cp 150 nodes 100000 nps 50000 pv 7g7f 3c3d 2g2f",
        );
    });

    it("info イベントを正しくフォーマットする（一部フィールドのみ）", () => {
        const event: EngineEvent = {
            type: "info",
            depth: 5,
            scoreCp: -200,
        };
        const result = formatEvent(event, "S:test");
        expect(result).toBe("[S:test] info depth 5 score cp -200");
    });

    it("info イベントで pv が空配列の場合は含めない", () => {
        const event: EngineEvent = {
            type: "info",
            depth: 3,
            pv: [],
        };
        const result = formatEvent(event, "G:test");
        expect(result).toBe("[G:test] info depth 3");
    });

    it("info イベントでフィールドが undefined の場合は含めない", () => {
        const event: EngineEvent = {
            type: "info",
        };
        const result = formatEvent(event, "S:engine");
        expect(result).toBe("[S:engine] info");
    });

    it("error イベントを正しくフォーマットする", () => {
        const event: EngineEvent = {
            type: "error",
            message: "Engine initialization failed",
        };
        const result = formatEvent(event, "G:engine3");
        expect(result).toBe("[G:engine3] error: Engine initialization failed");
    });

    it("ラベルが異なっても正しく動作する", () => {
        const event: EngineEvent = {
            type: "bestmove",
            move: "2g2f",
        };
        expect(formatEvent(event, "先手:内蔵エンジン")).toBe("[先手:内蔵エンジン] bestmove 2g2f");
        expect(formatEvent(event, "後手:外部エンジン")).toBe("[後手:外部エンジン] bestmove 2g2f");
    });
});

describe("useEngineManager", () => {
    const basePosition: PositionState = {
        board: {} as PositionState["board"],
        hands: { sente: {}, gote: {} },
        turn: "sente",
    };
    const timeSettings = {
        sente: { mainMs: 1000, byoyomiMs: 500 },
        gote: { mainMs: 1000, byoyomiMs: 500 },
    };

    const createMockEngineClient = () => {
        const listeners = new Set<(event: EngineEvent) => void>();
        const subscribe = vi.fn((handler: (event: EngineEvent) => void) => {
            listeners.add(handler);
            return () => listeners.delete(handler);
        });
        const init = vi.fn().mockResolvedValue(undefined);
        const loadPosition = vi.fn().mockResolvedValue(undefined);
        const cancel = vi.fn().mockResolvedValue(undefined);
        const search = vi.fn().mockResolvedValue({ cancel });
        const stop = vi.fn().mockResolvedValue(undefined);
        const dispose = vi.fn().mockResolvedValue(undefined);
        const emit = (event: EngineEvent) => {
            for (const fn of listeners) {
                fn(event);
            }
        };

        return {
            client: {
                init,
                loadPosition,
                search,
                stop,
                setOption: vi.fn().mockResolvedValue(undefined),
                subscribe,
                dispose,
            },
            emit,
            search,
            loadPosition,
        };
    };

    const renderEngineHook = ({
        positionRef,
        movesRef,
        onMoveFromEngine,
        onMatchEnd,
        sides,
        mockClient,
    }: {
        positionRef: MutableRefObject<PositionState>;
        movesRef: MutableRefObject<string[]>;
        onMoveFromEngine: (move: string) => void;
        onMatchEnd: (message: string) => Promise<void>;
        sides: {
            sente: { role: "human" | "engine"; engineId?: string };
            gote: { role: "human" | "engine"; engineId?: string };
        };
        mockClient: ReturnType<typeof createMockEngineClient>;
    }) => {
        return renderHook(() =>
            useEngineManager({
                sides,
                engineOptions: [
                    {
                        id: "engine1",
                        label: "Engine 1",
                        createClient: () => mockClient.client,
                    },
                ],
                timeSettings,
                startSfen: "startpos",
                movesRef,
                positionRef,
                isMatchRunning: true,
                positionReady: true,
                onMoveFromEngine,
                onMatchEnd,
                maxLogs: 10,
            }),
        );
    };

    afterEach(() => {
        vi.clearAllMocks();
    });

    it("エンジンを初期化し探索を開始する", async () => {
        const mockClient = createMockEngineClient();
        const positionRef = { current: { ...basePosition, turn: "sente" } };
        const movesRef = { current: [] as string[] };
        applyMoveWithStateMock.mockReturnValue({
            ok: true,
            next: positionRef.current,
        });
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);

        const { result } = renderEngineHook({
            positionRef,
            movesRef,
            onMoveFromEngine,
            onMatchEnd,
            sides: { sente: { role: "engine", engineId: "engine1" }, gote: { role: "human" } },
            mockClient,
        });

        await act(async () => {
            await Promise.resolve();
        });

        expect(mockClient.client.init).toHaveBeenCalledTimes(1);
        // init 時と探索開始時に局面を読み込む
        expect(mockClient.loadPosition).toHaveBeenCalledTimes(2);
        expect(mockClient.search).toHaveBeenCalledTimes(1);
        expect(result.current.engineStatus.sente).toBe("thinking");
        expect(result.current.engineReady.sente).toBe(true);
    });

    it("bestmove の通常手を適用してコールバックを呼び出す", async () => {
        const mockClient = createMockEngineClient();
        const positionRef = { current: { ...basePosition, turn: "sente" } };
        const movesRef = { current: [] as string[] };
        applyMoveWithStateMock.mockReturnValue({
            ok: true,
            next: positionRef.current,
        });
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);

        const { result } = renderEngineHook({
            positionRef,
            movesRef,
            onMoveFromEngine,
            onMatchEnd,
            sides: { sente: { role: "engine", engineId: "engine1" }, gote: { role: "human" } },
            mockClient,
        });

        await act(async () => {
            await Promise.resolve();
        });

        act(() => {
            mockClient.emit({ type: "bestmove", move: "7g7f" });
        });

        expect(onMoveFromEngine).toHaveBeenCalledWith("7g7f");
        expect(result.current.engineStatus.sente).toBe("idle");
    });

    it("bestmove の resign で対局終了コールバックを呼ぶ", async () => {
        const mockClient = createMockEngineClient();
        const positionRef = { current: { ...basePosition, turn: "sente" } };
        const movesRef = { current: [] as string[] };
        applyMoveWithStateMock.mockReturnValue({
            ok: true,
            next: positionRef.current,
        });
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);

        renderEngineHook({
            positionRef,
            movesRef,
            onMoveFromEngine,
            onMatchEnd,
            sides: { sente: { role: "engine", engineId: "engine1" }, gote: { role: "human" } },
            mockClient,
        });

        await act(async () => {
            await Promise.resolve();
        });

        act(() => {
            mockClient.emit({ type: "bestmove", move: "resign" });
        });

        expect(onMatchEnd).toHaveBeenCalledTimes(1);
        const message = onMatchEnd.mock.calls[0][0] as string;
        expect(message).toContain("投了しました");
    });
});
