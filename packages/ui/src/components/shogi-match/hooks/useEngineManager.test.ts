import type { GameResult, NnueSelection, Player } from "@shogi/app-core";
import type { EngineEvent } from "@shogi/engine-client";
import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { determineBestmoveAction, formatEvent, useEngineManager } from "./useEngineManager";

// テスト用のNnueSelection作成ヘルパー
const createNnueSelection = (nnueId: string | null): NnueSelection => ({
    presetKey: null,
    nnueId,
});

// テスト用のresolveNnueモック
const createMockResolveNnue = () => vi.fn(async (selection: NnueSelection) => selection.nnueId);

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
    const timeSettings = {
        sente: { mainMs: 1000, byoyomiMs: 500 },
        gote: { mainMs: 1000, byoyomiMs: 500 },
    };
    const createMockClocksRef = () => ({
        current: {
            sente: { mainMs: 1000, byoyomiMs: 500 },
            gote: { mainMs: 1000, byoyomiMs: 500 },
            ticking: null as "sente" | "gote" | null,
            lastUpdatedAt: Date.now(),
        },
    });

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
        positionTurn,
        movesRef,
        onMoveFromEngine,
        onMatchEnd,
        sides,
        mockClient,
        clocksRef = createMockClocksRef(),
    }: {
        positionTurn: Player;
        movesRef: { current: string[] };
        onMoveFromEngine: (move: string) => void;
        onMatchEnd: (result: GameResult) => Promise<void>;
        sides: {
            sente: { role: "human" | "engine"; engineId?: string };
            gote: { role: "human" | "engine"; engineId?: string };
        };
        mockClient: ReturnType<typeof createMockEngineClient>;
        clocksRef?: ReturnType<typeof createMockClocksRef>;
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
                clocksRef,
                startSfen: "startpos",
                movesRef,
                positionTurn,
                isMatchRunning: true,
                positionReady: true,
                onMoveFromEngine,
                onMatchEnd,
                maxLogs: 10,
                senteNnueSelection: createNnueSelection(null),
                goteNnueSelection: createNnueSelection(null),
                resolveNnue: createMockResolveNnue(),
            }),
        );
    };

    afterEach(() => {
        vi.clearAllMocks();
    });

    it("エンジンを初期化し探索を開始する", async () => {
        const mockClient = createMockEngineClient();
        const movesRef = { current: [] as string[] };
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);

        const { result } = renderEngineHook({
            positionTurn: "sente",
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
        const movesRef = { current: [] as string[] };
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);

        const { result } = renderEngineHook({
            positionTurn: "sente",
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
        const movesRef = { current: [] as string[] };
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);

        renderEngineHook({
            positionTurn: "sente",
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
        const gameResult = onMatchEnd.mock.calls[0][0] as GameResult;
        expect(gameResult.reason.kind).toBe("resignation");
    });
});

describe("useEngineManager - NNUE restart", () => {
    const timeSettings = {
        sente: { mainMs: 1000, byoyomiMs: 500 },
        gote: { mainMs: 1000, byoyomiMs: 500 },
    };
    const createMockClocksRef = () => ({
        current: {
            sente: { mainMs: 1000, byoyomiMs: 500 },
            gote: { mainMs: 1000, byoyomiMs: 500 },
            ticking: null as "sente" | "gote" | null,
            lastUpdatedAt: Date.now(),
        },
    });

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
        const reset = vi.fn().mockResolvedValue(undefined);
        const loadNnue = vi.fn().mockResolvedValue(undefined);

        return {
            client: {
                init,
                loadPosition,
                search,
                stop,
                setOption: vi.fn().mockResolvedValue(undefined),
                subscribe,
                dispose,
                reset,
                loadNnue,
            },
            init,
            reset,
            loadNnue,
            loadPosition,
        };
    };

    afterEach(() => {
        vi.clearAllMocks();
    });

    it("NNUE ID変更時にエンジンを再起動する", async () => {
        const mockClient = createMockEngineClient();
        const movesRef = { current: [] as string[] };
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);
        const resolveNnue = createMockResolveNnue();

        const { rerender } = renderHook(
            ({ senteNnueSelection }: { senteNnueSelection: NnueSelection }) =>
                useEngineManager({
                    sides: {
                        sente: { role: "engine", engineId: "engine1" },
                        gote: { role: "human" },
                    },
                    engineOptions: [
                        {
                            id: "engine1",
                            label: "Engine 1",
                            createClient: () => mockClient.client,
                        },
                    ],
                    timeSettings,
                    clocksRef: createMockClocksRef(),
                    startSfen: "startpos",
                    movesRef,
                    positionTurn: "sente",
                    isMatchRunning: false,
                    positionReady: true,
                    onMoveFromEngine,
                    onMatchEnd,
                    maxLogs: 10,
                    senteNnueSelection,
                    goteNnueSelection: createNnueSelection(null),
                    resolveNnue,
                }),
            { initialProps: { senteNnueSelection: createNnueSelection("nnue-1") } },
        );

        // 初期化を待つ
        await act(async () => {
            await Promise.resolve();
        });

        // 初期状態を確認
        expect(mockClient.init).toHaveBeenCalled();
        const initCallCount = mockClient.init.mock.calls.length;

        // NNUE ID を変更
        rerender({ senteNnueSelection: createNnueSelection("nnue-2") });

        // useEffect が実行されるのを待つ
        await act(async () => {
            await Promise.resolve();
            await Promise.resolve();
        });

        // reset と init が追加で呼ばれることを確認
        expect(mockClient.reset).toHaveBeenCalled();
        expect(mockClient.init.mock.calls.length).toBeGreaterThan(initCallCount);
    });

    it("undefined→nullの変更時は再起動しない（NNUEなし→なし）", async () => {
        const mockClient = createMockEngineClient();
        const movesRef = { current: [] as string[] };
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);
        const resolveNnue = createMockResolveNnue();

        const { rerender } = renderHook(
            ({ senteNnueSelection }: { senteNnueSelection: NnueSelection | undefined }) =>
                useEngineManager({
                    sides: {
                        sente: { role: "engine", engineId: "engine1" },
                        gote: { role: "human" },
                    },
                    engineOptions: [
                        {
                            id: "engine1",
                            label: "Engine 1",
                            createClient: () => mockClient.client,
                        },
                    ],
                    timeSettings,
                    clocksRef: createMockClocksRef(),
                    startSfen: "startpos",
                    movesRef,
                    positionTurn: "sente",
                    isMatchRunning: false,
                    positionReady: true,
                    onMoveFromEngine,
                    onMatchEnd,
                    maxLogs: 10,
                    senteNnueSelection,
                    goteNnueSelection: createNnueSelection(null),
                    resolveNnue,
                }),
            { initialProps: { senteNnueSelection: undefined as NnueSelection | undefined } },
        );

        // 初期化を待つ
        await act(async () => {
            await Promise.resolve();
        });

        const resetCallCount = mockClient.reset.mock.calls.length;

        // undefined → NnueSelection(null) に変更（どちらも「NNUEなし」を意味）
        rerender({ senteNnueSelection: createNnueSelection(null) });

        await act(async () => {
            await Promise.resolve();
            await Promise.resolve();
        });

        // reset が追加で呼ばれないことを確認
        expect(mockClient.reset.mock.calls.length).toBe(resetCallCount);
    });

    it("対局中はNNUE変更時に再起動しない", async () => {
        const mockClient = createMockEngineClient();
        const movesRef = { current: [] as string[] };
        const onMoveFromEngine = vi.fn();
        const onMatchEnd = vi.fn().mockResolvedValue(undefined);
        const resolveNnue = createMockResolveNnue();

        const { rerender } = renderHook(
            ({ senteNnueSelection }: { senteNnueSelection: NnueSelection }) =>
                useEngineManager({
                    sides: {
                        sente: { role: "engine", engineId: "engine1" },
                        gote: { role: "human" },
                    },
                    engineOptions: [
                        {
                            id: "engine1",
                            label: "Engine 1",
                            createClient: () => mockClient.client,
                        },
                    ],
                    timeSettings,
                    clocksRef: createMockClocksRef(),
                    startSfen: "startpos",
                    movesRef,
                    positionTurn: "sente",
                    isMatchRunning: true, // 対局中
                    positionReady: true,
                    onMoveFromEngine,
                    onMatchEnd,
                    maxLogs: 10,
                    senteNnueSelection,
                    goteNnueSelection: createNnueSelection(null),
                    resolveNnue,
                }),
            { initialProps: { senteNnueSelection: createNnueSelection("nnue-1") } },
        );

        // 初期化を待つ
        await act(async () => {
            await Promise.resolve();
        });

        const resetCallCount = mockClient.reset.mock.calls.length;

        // NNUE ID を変更
        rerender({ senteNnueSelection: createNnueSelection("nnue-2") });

        await act(async () => {
            await Promise.resolve();
            await Promise.resolve();
        });

        // 対局中なので reset が追加で呼ばれないことを確認
        expect(mockClient.reset.mock.calls.length).toBe(resetCallCount);
    });
});

describe("determineBestmoveAction", () => {
    it("通常の手の場合、apply_moveアクションを返す", () => {
        const result = determineBestmoveAction({
            move: "7g7f",
            side: "sente",
            engineId: "engine1",
            activeSearch: { side: "sente", engineId: "engine1" },
            movesCount: 5,
        });

        expect(result.action).toBe("apply_move");
        expect(result.move).toBe("7g7f");
        expect(result.shouldClearActive).toBe(true);
        expect(result.shouldUpdateRequestPly).toBe(true);
    });

    it("win トークンの場合、end_matchアクションを返す", () => {
        const result = determineBestmoveAction({
            move: "win",
            side: "sente",
            engineId: "engine1",
            activeSearch: { side: "sente", engineId: "engine1" },
            movesCount: 5,
        });

        expect(result.action).toBe("end_match");
        expect(result.gameResult?.reason.kind).toBe("win_declaration");
        expect(result.shouldClearActive).toBe(true);
        expect(result.shouldUpdateRequestPly).toBe(true);
    });

    it("resign トークンの場合、end_matchアクションを返す", () => {
        const result = determineBestmoveAction({
            move: "resign",
            side: "sente",
            engineId: "engine1",
            activeSearch: { side: "sente", engineId: "engine1" },
            movesCount: 5,
        });

        expect(result.action).toBe("end_match");
        expect(result.gameResult?.reason.kind).toBe("resignation");
        expect(result.shouldClearActive).toBe(true);
    });

    it("none トークンの場合、end_matchアクションを返す", () => {
        const result = determineBestmoveAction({
            move: "none",
            side: "gote",
            engineId: "engine1",
            activeSearch: { side: "gote", engineId: "engine1" },
            movesCount: 5,
        });

        expect(result.action).toBe("end_match");
        expect(result.gameResult?.reason.kind).toBe("checkmate");
        expect(result.shouldClearActive).toBe(true);
    });

    it("activeSearchが一致しない場合、skipアクションを返す", () => {
        const result = determineBestmoveAction({
            move: "7g7f",
            side: "sente",
            engineId: "engine1",
            activeSearch: { side: "gote", engineId: "engine1" },
            movesCount: 5,
        });

        expect(result.action).toBe("skip");
        expect(result.shouldClearActive).toBe(false);
        expect(result.shouldUpdateRequestPly).toBe(false);
    });

    it("activeSearchがnullの場合、skipアクションを返す", () => {
        const result = determineBestmoveAction({
            move: "7g7f",
            side: "sente",
            engineId: "engine1",
            activeSearch: null,
            movesCount: 5,
        });

        expect(result.action).toBe("skip");
        expect(result.shouldClearActive).toBe(false);
    });

    it("engineIdが一致しない場合、skipアクションを返す", () => {
        const result = determineBestmoveAction({
            move: "7g7f",
            side: "sente",
            engineId: "engine1",
            activeSearch: { side: "sente", engineId: "engine2" },
            movesCount: 5,
        });

        expect(result.action).toBe("skip");
        expect(result.shouldClearActive).toBe(false);
    });

    it("大文字小文字を区別せずにトークンを判定する", () => {
        const resultWin = determineBestmoveAction({
            move: "WIN",
            side: "sente",
            engineId: "engine1",
            activeSearch: { side: "sente", engineId: "engine1" },
            movesCount: 5,
        });

        expect(resultWin.action).toBe("end_match");
        expect(resultWin.gameResult?.reason.kind).toBe("win_declaration");

        const resultResign = determineBestmoveAction({
            move: "RESIGN",
            side: "gote",
            engineId: "engine1",
            activeSearch: { side: "gote", engineId: "engine1" },
            movesCount: 5,
        });

        expect(resultResign.action).toBe("end_match");
        expect(resultResign.gameResult?.reason.kind).toBe("resignation");
    });

    it("空白を含む手をトリムする", () => {
        const result = determineBestmoveAction({
            move: "  7g7f  ",
            side: "sente",
            engineId: "engine1",
            activeSearch: { side: "sente", engineId: "engine1" },
            movesCount: 5,
        });

        expect(result.action).toBe("apply_move");
        expect(result.move).toBe("7g7f");
    });
});
