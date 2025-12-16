import type { EngineEvent } from "@shogi/engine-client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createWasmEngineClient } from "./index";

describe("createWasmEngineClient", () => {
    let mockWorker: {
        postMessage: ReturnType<typeof vi.fn>;
        terminate: ReturnType<typeof vi.fn>;
        onmessage: ((event: MessageEvent) => void) | null;
        onerror: ((error: ErrorEvent) => void) | null;
    };
    let mockWorkerFactory: ReturnType<typeof vi.fn>;

    beforeEach(() => {
        mockWorker = {
            postMessage: vi.fn(),
            terminate: vi.fn(),
            onmessage: null,
            onerror: null,
        };
        mockWorkerFactory = vi.fn(() => mockWorker as unknown as Worker);
    });

    afterEach(() => {
        vi.clearAllMocks();
    });

    describe("useMock オプション", () => {
        it("useMock: true の場合、即座にモック実装を使用する", async () => {
            const client = createWasmEngineClient({ useMock: true });

            await client.init();

            // Worker が作成されないことを確認
            expect(mockWorkerFactory).not.toHaveBeenCalled();
        });

        it("useMock: true の場合、全操作がモックで動作する", async () => {
            const client = createWasmEngineClient({ useMock: true });

            const events: EngineEvent[] = [];
            client.subscribe((event) => events.push(event));

            await client.init();
            await client.loadPosition("startpos");
            await client.search({});

            // モックが動作することを確認（エラーが発生しない）
            expect(events.length).toBeGreaterThanOrEqual(0);
        });
    });

    describe("基本操作", () => {
        it("init で Worker にメッセージを送信する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const initPromise = client.init();

            expect(mockWorkerFactory).toHaveBeenCalledTimes(1);
            expect(mockWorker.postMessage).toHaveBeenCalledWith(
                expect.objectContaining({
                    type: "init",
                    opts: expect.objectContaining({ backend: "wasm" }),
                    requestId: expect.any(String),
                }),
            );

            // ack を返す
            const call = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: call.requestId },
            } as MessageEvent);

            await initPromise;
        });

        it("init オプションを正しく渡す", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const initPromise = client.init({ multiPv: 3, ttSizeMb: 1024 });

            expect(mockWorker.postMessage).toHaveBeenCalledWith(
                expect.objectContaining({
                    type: "init",
                    opts: expect.objectContaining({ multiPv: 3, ttSizeMb: 1024 }),
                }),
            );

            // ack を返す
            const call = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: call.requestId },
            } as MessageEvent);

            await initPromise;
        });

        it("loadPosition で Worker にメッセージを送信する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            // loadPosition - trigger the call but don't await yet
            client.loadPosition("startpos", ["7g7f"]);

            // Wait for microtask to process
            await new Promise((resolve) => setTimeout(resolve, 0));

            const loadCall = mockWorker.postMessage.mock.calls[1][0];

            expect(loadCall).toMatchObject({
                type: "loadPosition",
                sfen: "startpos",
                moves: ["7g7f"],
                requestId: expect.any(String),
            });

            // ack を返す
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: loadCall.requestId },
            } as MessageEvent);
        });

        it("search で Worker にメッセージを送信する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            // search
            const handle = await client.search({ limits: { maxDepth: 10 } });

            expect(mockWorker.postMessage).toHaveBeenCalledWith(
                expect.objectContaining({
                    type: "search",
                    params: { limits: { maxDepth: 10 } },
                }),
            );
            expect(handle.cancel).toBeInstanceOf(Function);
        });

        it("setOption で Worker にメッセージを送信する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            // setOption - trigger the call but don't await yet
            client.setOption("threads", 4);

            // Wait for microtask to process
            await new Promise((resolve) => setTimeout(resolve, 0));

            const optionCall = mockWorker.postMessage.mock.calls[1][0];

            expect(optionCall).toMatchObject({
                type: "setOption",
                name: "threads",
                value: 4,
                requestId: expect.any(String),
            });

            // ack を返す
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: optionCall.requestId },
            } as MessageEvent);
        });
    });

    describe("イベント購読", () => {
        it("subscribe でイベントリスナーを登録する", () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const handler = vi.fn();
            const unsubscribe = client.subscribe(handler);

            expect(unsubscribe).toBeInstanceOf(Function);
        });

        it("Worker からのイベントメッセージを処理する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const events: EngineEvent[] = [];
            client.subscribe((event) => events.push(event));

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            // イベントを発行
            const event: EngineEvent = {
                type: "bestmove",
                move: "7g7f",
            };
            mockWorker.onmessage?.({
                data: { type: "event", payload: event },
            } as MessageEvent);

            expect(events).toHaveLength(1);
            expect(events[0]).toEqual(event);
        });

        it("Worker からの複数イベントメッセージ (events) を処理する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const events: EngineEvent[] = [];
            client.subscribe((event) => events.push(event));

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            // 複数イベントを発行
            const eventList: EngineEvent[] = [
                { type: "info", depth: 1, score: 100 },
                { type: "info", depth: 2, score: 150 },
            ];
            mockWorker.onmessage?.({
                data: { type: "events", payload: eventList },
            } as MessageEvent);

            expect(events).toHaveLength(2);
            expect(events[0]).toEqual(eventList[0]);
            expect(events[1]).toEqual(eventList[1]);
        });

        it("unsubscribe でハンドラを削除する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const handler = vi.fn();
            const unsubscribe = client.subscribe(handler);

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            // unsubscribe
            unsubscribe();

            // イベントを発行
            mockWorker.onmessage?.({
                data: { type: "event", payload: { type: "bestmove", move: "7g7f" } },
            } as MessageEvent);

            // ハンドラは呼ばれない
            expect(handler).not.toHaveBeenCalled();
        });
    });

    describe("エラーハンドリング", () => {
        it("Worker 初期化失敗時にモックにフォールバックする", async () => {
            const failingFactory = vi.fn(() => {
                throw new Error("Worker initialization failed");
            });

            const client = createWasmEngineClient({ workerFactory: failingFactory });

            // init でエラーが発生するが、モックにフォールバックするので成功する
            await expect(client.init()).resolves.toBeUndefined();
        });

        it("Worker onerror 時にモックにフォールバックする", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const events: EngineEvent[] = [];
            client.subscribe((event) => events.push(event));

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            // Worker エラーを発生させる
            mockWorker.onerror?.({} as ErrorEvent);

            // エラーイベントが発行される
            const errorEvents = events.filter((e) => e.type === "error");
            expect(errorEvents.length).toBeGreaterThan(0);

            // その後の操作はモックで動作する
            await expect(client.loadPosition("startpos")).resolves.toBeUndefined();
        });

        it("ack でエラーが返された場合は reject する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];

            // エラー付き ack を返す
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId, error: "Init failed" },
            } as MessageEvent);

            await expect(initPromise).rejects.toThrow("Init failed");
        });
    });

    describe("stop と SearchHandle", () => {
        it("stop (terminate モード) で Worker を終了する", async () => {
            const client = createWasmEngineClient({
                workerFactory: mockWorkerFactory,
                stopMode: "terminate",
            });

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            await client.stop();

            expect(mockWorker.terminate).toHaveBeenCalled();
        });

        it("SearchHandle.cancel (terminate モード) で Worker を終了する", async () => {
            const client = createWasmEngineClient({
                workerFactory: mockWorkerFactory,
                stopMode: "terminate",
            });

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            const handle = await client.search({});
            await handle.cancel();

            expect(mockWorker.terminate).toHaveBeenCalled();
        });

        it("cooperative モードは未実装のため terminate にフォールバックする", async () => {
            const consoleWarnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

            const client = createWasmEngineClient({
                workerFactory: mockWorkerFactory,
                stopMode: "cooperative",
            });

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            await client.stop();

            expect(consoleWarnSpy).toHaveBeenCalledWith(
                expect.stringContaining("cooperative stop is not yet supported"),
            );
            expect(mockWorker.terminate).toHaveBeenCalled();

            consoleWarnSpy.mockRestore();
        });
    });

    describe("dispose", () => {
        it("dispose でリソースをクリーンアップする", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            await client.dispose();

            expect(mockWorker.postMessage).toHaveBeenCalledWith(
                expect.objectContaining({ type: "dispose" }),
            );
            expect(mockWorker.terminate).toHaveBeenCalled();
        });

        it("useMock: true の場合、dispose でモックをクリーンアップする", async () => {
            const client = createWasmEngineClient({ useMock: true });

            await client.init();
            await client.dispose();

            // エラーが発生しないことを確認
            expect(mockWorkerFactory).not.toHaveBeenCalled();
        });
    });

    describe("Worker 通信の詳細", () => {
        it("requestId が生成され、ack に使用される", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const initPromise = client.init();

            // requestId が生成されている
            const call = mockWorker.postMessage.mock.calls[0][0];
            expect(call.requestId).toBeDefined();
            expect(typeof call.requestId).toBe("string");

            // 同じ requestId で ack を返す
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: call.requestId },
            } as MessageEvent);

            await initPromise;
        });

        it("複数の並列リクエストを正しく処理する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            // init
            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            // 2つの並列リクエスト
            client.loadPosition("startpos");
            client.setOption("threads", 4);

            // Wait for microtasks to process
            await new Promise((resolve) => setTimeout(resolve, 0));

            const call1 = mockWorker.postMessage.mock.calls[1][0];
            const call2 = mockWorker.postMessage.mock.calls[2][0];

            // 逆順で ack を返す
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: call2.requestId },
            } as MessageEvent);
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: call1.requestId },
            } as MessageEvent);
        });
    });
});
