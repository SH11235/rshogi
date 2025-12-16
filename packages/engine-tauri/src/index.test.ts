import type { EngineEvent } from "@shogi/engine-client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createTauriEngineClient, getLegalMoves } from "./index";

describe("createTauriEngineClient", () => {
    let mockInvoke: ReturnType<typeof vi.fn>;
    let mockListen: ReturnType<typeof vi.fn>;
    let mockUnlisten: ReturnType<typeof vi.fn>;

    beforeEach(() => {
        mockInvoke = vi.fn();
        mockListen = vi.fn();
        mockUnlisten = vi.fn();
    });

    afterEach(() => {
        vi.clearAllMocks();
    });

    describe("基本操作", () => {
        it("init で IPC を呼び出す", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockResolvedValue(mockUnlisten);

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            await client.init();

            expect(mockInvoke).toHaveBeenCalledWith("engine_init", { opts: undefined });
            expect(mockListen).toHaveBeenCalledWith("engine://event", expect.any(Function));
        });

        it("init オプションを正しく渡す", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockResolvedValue(mockUnlisten);

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
                threads: 4,
                ttSizeMb: 1024,
            });

            await client.init({ multiPv: 3 });

            expect(mockInvoke).toHaveBeenCalledWith("engine_init", {
                opts: { multiPv: 3 },
            });
        });

        it("loadPosition で IPC を呼び出す", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockResolvedValue(mockUnlisten);

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            await client.loadPosition("startpos", ["7g7f"]);

            expect(mockInvoke).toHaveBeenCalledWith("engine_position", {
                sfen: "startpos",
                moves: ["7g7f"],
            });
        });

        it("search で IPC を呼び出す", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockResolvedValue(mockUnlisten);

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            await client.init();
            const handle = await client.search({ limits: { maxDepth: 10 } });

            expect(mockInvoke).toHaveBeenCalledWith("engine_search", {
                params: { limits: { maxDepth: 10 } },
            });
            expect(handle.cancel).toBeInstanceOf(Function);
        });

        it("stop で IPC を呼び出す", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockResolvedValue(mockUnlisten);

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            await client.stop();

            expect(mockInvoke).toHaveBeenCalledWith("engine_stop");
        });

        it("setOption で IPC を呼び出す", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockResolvedValue(mockUnlisten);

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            await client.setOption("threads", 4);

            expect(mockInvoke).toHaveBeenCalledWith("engine_option", {
                name: "threads",
                value: 4,
            });
        });
    });

    describe("イベント購読", () => {
        it("subscribe でイベントリスナーを登録する", () => {
            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            const handler = vi.fn();
            const unsubscribe = client.subscribe(handler);

            expect(unsubscribe).toBeInstanceOf(Function);
        });

        it("イベントが発行されたらハンドラを呼び出す", async () => {
            let eventCallback: ((evt: { payload: EngineEvent }) => void) | null = null;

            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockImplementation((_eventName, callback) => {
                eventCallback = callback;
                return Promise.resolve(mockUnlisten);
            });

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            const handler = vi.fn();
            client.subscribe(handler);

            await client.init();

            // イベントを発行
            const event: EngineEvent = {
                type: "bestmove",
                move: "7g7f",
            };
            eventCallback?.({ payload: event });

            expect(handler).toHaveBeenCalledWith(event);
        });

        it("unsubscribe でハンドラを削除する", async () => {
            let eventCallback: ((evt: { payload: EngineEvent }) => void) | null = null;

            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockImplementation((_eventName, callback) => {
                eventCallback = callback;
                return Promise.resolve(mockUnlisten);
            });

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            const handler = vi.fn();
            const unsubscribe = client.subscribe(handler);

            await client.init();

            // unsubscribe を呼び出す
            unsubscribe();

            // イベントを発行
            const event: EngineEvent = {
                type: "bestmove",
                move: "7g7f",
            };
            eventCallback?.({ payload: event });

            // ハンドラは呼ばれない
            expect(handler).not.toHaveBeenCalled();
        });
    });

    describe("エラーハンドリング", () => {
        it("useMockOnError: true の場合、IPC エラー時にモックにフォールバックする", async () => {
            mockInvoke.mockRejectedValue(new Error("IPC failed"));

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
                useMockOnError: true,
            });

            // エラーは投げられず、モックが動作する
            await expect(client.init()).resolves.toBeUndefined();
            await expect(client.loadPosition("startpos")).resolves.toBeUndefined();
        });

        it("useMockOnError: false の場合、IPC エラーを投げる", async () => {
            mockInvoke.mockRejectedValue(new Error("IPC failed"));

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
                useMockOnError: false,
            });

            // エラーが投げられる
            await expect(client.init()).rejects.toThrow("IPC failed");
        });

        it("listen エラー時にモックにフォールバックする (useMockOnError: true)", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockRejectedValue(new Error("Listen failed"));

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
                useMockOnError: true,
            });

            const handler = vi.fn();
            client.subscribe(handler);

            // init で listen が失敗するが、モックにフォールバック
            await expect(client.init()).resolves.toBeUndefined();
        });
    });

    describe("dispose", () => {
        it("dispose でリソースをクリーンアップする", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockResolvedValue(mockUnlisten);

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
            });

            await client.init();
            await client.dispose();

            expect(mockInvoke).toHaveBeenCalledWith("engine_stop");
            expect(mockUnlisten).toHaveBeenCalled();
        });
    });

    describe("カスタムイベント名", () => {
        it("eventName オプションで独自のイベント名を使用できる", async () => {
            mockInvoke.mockResolvedValue(undefined);
            mockListen.mockResolvedValue(mockUnlisten);

            const client = createTauriEngineClient({
                ipc: { invoke: mockInvoke, listen: mockListen },
                eventName: "custom://event",
            });

            await client.init();

            expect(mockListen).toHaveBeenCalledWith("custom://event", expect.any(Function));
        });
    });
});

describe("getLegalMoves", () => {
    let mockInvoke: ReturnType<typeof vi.fn>;

    beforeEach(() => {
        mockInvoke = vi.fn();
    });

    afterEach(() => {
        vi.clearAllMocks();
    });

    it("IPC 経由で合法手を取得する", async () => {
        mockInvoke.mockResolvedValue(["7g7f", "2g2f", "6g6f"]);

        const result = await getLegalMoves({
            sfen: "startpos",
            ipc: { invoke: mockInvoke },
        });

        expect(mockInvoke).toHaveBeenCalledWith("engine_legal_moves", {
            sfen: "startpos",
            moves: undefined,
        });
        expect(result).toEqual(["7g7f", "2g2f", "6g6f"]);
    });

    it("moves パラメータを正しく渡す", async () => {
        mockInvoke.mockResolvedValue(["3c3d", "8c8d"]);

        const result = await getLegalMoves({
            sfen: "startpos",
            moves: ["7g7f"],
            ipc: { invoke: mockInvoke },
        });

        expect(mockInvoke).toHaveBeenCalledWith("engine_legal_moves", {
            sfen: "startpos",
            moves: ["7g7f"],
        });
        expect(result).toEqual(["3c3d", "8c8d"]);
    });

    it("エラー時は空配列を返す", async () => {
        mockInvoke.mockRejectedValue(new Error("Failed to get legal moves"));

        const result = await getLegalMoves({
            sfen: "startpos",
            ipc: { invoke: mockInvoke },
        });

        expect(result).toEqual([]);
    });
});
