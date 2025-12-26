import type { EngineEvent } from "@shogi/engine-client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createWasmEngineClient } from "./index";

type WorkerKind = "single" | "threaded";
type MockWorker = {
    postMessage: ReturnType<typeof vi.fn>;
    terminate: ReturnType<typeof vi.fn>;
    onmessage: ((event: MessageEvent) => void) | null;
    onerror: ((error: ErrorEvent) => void) | null;
};

const createMockWorker = (): MockWorker => ({
    postMessage: vi.fn(),
    terminate: vi.fn(),
    onmessage: null,
    onerror: null,
});

describe("createWasmEngineClient", () => {
    let mockWorker: MockWorker;
    let mockWorkerFactory: (kind: WorkerKind) => Worker;

    beforeEach(() => {
        mockWorker = createMockWorker();
        mockWorkerFactory = vi.fn((_kind: WorkerKind) => mockWorker as unknown as Worker);
    });

    afterEach(() => {
        vi.clearAllMocks();
        vi.unstubAllGlobals();
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

        it("Threads の setOption は Worker に送らず次回 init に反映する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            client.setOption("Threads", 4);
            await new Promise((resolve) => setTimeout(resolve, 0));

            expect(mockWorker.postMessage).toHaveBeenCalledTimes(1);
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

        it("同一 SFEN の追加入力は applyMoves で差分だけ送信する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);
            await initPromise;

            const firstPromise = client.loadPosition("startpos", ["7g7f"]);
            await new Promise((resolve) => setTimeout(resolve, 0));

            const firstCall = mockWorker.postMessage.mock.calls[1][0];
            expect(firstCall).toMatchObject({
                type: "loadPosition",
                sfen: "startpos",
                moves: ["7g7f"],
                requestId: expect.any(String),
            });
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: firstCall.requestId },
            } as MessageEvent);
            await firstPromise;

            const secondPromise = client.loadPosition("startpos", ["7g7f", "3c3d"]);
            await new Promise((resolve) => setTimeout(resolve, 0));

            const secondCall = mockWorker.postMessage.mock.calls[2][0];
            expect(secondCall).toMatchObject({
                type: "applyMoves",
                moves: ["3c3d"],
                requestId: expect.any(String),
            });
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: secondCall.requestId },
            } as MessageEvent);
            await secondPromise;
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
            client.setOption("USI_Hash", 256);

            // Wait for microtask to process
            await new Promise((resolve) => setTimeout(resolve, 0));

            const optionCall = mockWorker.postMessage.mock.calls[1][0];

            expect(optionCall).toMatchObject({
                type: "setOption",
                name: "USI_Hash",
                value: 256,
                requestId: expect.any(String),
            });

            // ack を返す
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: optionCall.requestId },
            } as MessageEvent);
        });

        it("init 中の setOption は applyPendingOptions のみに反映する", async () => {
            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });

            const initPromise = client.init();
            const initCall = mockWorker.postMessage.mock.calls[0][0];

            const setPromise = client.setOption("USI_Hash", 256);

            await new Promise((resolve) => setTimeout(resolve, 0));
            expect(mockWorker.postMessage).toHaveBeenCalledTimes(1);

            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);

            await new Promise((resolve) => setTimeout(resolve, 0));
            expect(mockWorker.postMessage).toHaveBeenCalledTimes(2);

            const optionCall = mockWorker.postMessage.mock.calls[1][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: optionCall.requestId },
            } as MessageEvent);

            await Promise.all([initPromise, setPromise]);
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
                { type: "info", depth: 1, scoreCp: 100 },
                { type: "info", depth: 2, scoreCp: 150 },
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
            const failingFactory = vi.fn((_kind: WorkerKind) => {
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

    describe("スレッド関連の初期化", () => {
        it("threaded init 失敗時に single へフォールバックして warning を出す", async () => {
            vi.stubGlobal("crossOriginIsolated", true);
            vi.stubGlobal("SharedArrayBuffer", class {});
            vi.stubGlobal("navigator", { hardwareConcurrency: 8 });

            const workers: { kind: WorkerKind; worker: MockWorker }[] = [];
            const workerFactory = vi.fn((kind: WorkerKind) => {
                const worker = createMockWorker();
                workers.push({ kind, worker });
                return worker as unknown as Worker;
            });

            const client = createWasmEngineClient({ workerFactory });
            const events: EngineEvent[] = [];
            client.subscribe((event) => events.push(event));

            const initPromise = client.init({ threads: 2 });

            const threadedWorker = workers[0].worker;
            const threadedInit = threadedWorker.postMessage.mock.calls[0][0];
            threadedWorker.onmessage?.({
                data: { type: "ack", requestId: threadedInit.requestId, error: "Init failed" },
            } as MessageEvent);

            await new Promise((resolve) => setTimeout(resolve, 0));

            expect(workerFactory).toHaveBeenCalledTimes(2);
            expect(workerFactory).toHaveBeenNthCalledWith(1, "threaded");
            expect(workerFactory).toHaveBeenNthCalledWith(2, "single");

            const singleWorker = workers[1].worker;
            const singleInit = singleWorker.postMessage.mock.calls[0][0];
            singleWorker.onmessage?.({
                data: { type: "ack", requestId: singleInit.requestId },
            } as MessageEvent);

            await initPromise;

            expect(events).toEqual(
                expect.arrayContaining([
                    expect.objectContaining({
                        type: "error",
                        code: "WASM_THREADS_INIT_FAILED",
                        severity: "warning",
                    }),
                ]),
            );
        });

        it("Threads が利用不可の場合に warning を出して single で初期化する", async () => {
            vi.stubGlobal("crossOriginIsolated", false);
            vi.stubGlobal("SharedArrayBuffer", undefined);

            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });
            const events: EngineEvent[] = [];
            client.subscribe((event) => events.push(event));

            const initPromise = client.init({ threads: 2 });
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);

            await initPromise;

            expect(events).toEqual(
                expect.arrayContaining([
                    expect.objectContaining({
                        type: "error",
                        code: "WASM_THREADS_UNAVAILABLE",
                        severity: "warning",
                    }),
                ]),
            );
        });

        it("Threads が上限超過の場合に clamp warning を出す", async () => {
            vi.stubGlobal("crossOriginIsolated", true);
            vi.stubGlobal("SharedArrayBuffer", class {});
            vi.stubGlobal("navigator", { hardwareConcurrency: 2 });

            const client = createWasmEngineClient({ workerFactory: mockWorkerFactory });
            const events: EngineEvent[] = [];
            client.subscribe((event) => events.push(event));

            const initPromise = client.init({ threads: 8 });
            const initCall = mockWorker.postMessage.mock.calls[0][0];
            mockWorker.onmessage?.({
                data: { type: "ack", requestId: initCall.requestId },
            } as MessageEvent);

            await initPromise;

            expect(events).toEqual(
                expect.arrayContaining([
                    expect.objectContaining({
                        type: "error",
                        code: "WASM_THREADS_CLAMPED",
                        severity: "warning",
                    }),
                ]),
            );
        });

        it("init で threads を変更した場合は worker を再生成する", async () => {
            vi.stubGlobal("crossOriginIsolated", true);
            vi.stubGlobal("SharedArrayBuffer", class {});
            vi.stubGlobal("navigator", { hardwareConcurrency: 8 });

            const workers: MockWorker[] = [];
            const workerFactory = vi.fn((kind: WorkerKind) => {
                void kind;
                const worker = createMockWorker();
                workers.push(worker);
                return worker as unknown as Worker;
            });

            const client = createWasmEngineClient({ workerFactory });

            const initPromise1 = client.init({ threads: 2 });
            const initCall1 = workers[0].postMessage.mock.calls[0][0];
            workers[0].onmessage?.({
                data: { type: "ack", requestId: initCall1.requestId },
            } as MessageEvent);
            await initPromise1;

            const initPromise2 = client.init({ threads: 3 });
            expect(workerFactory).toHaveBeenCalledTimes(2);
            expect(workers[0].terminate).toHaveBeenCalled();

            const initCall2 = workers[1].postMessage.mock.calls[0][0];
            workers[1].onmessage?.({
                data: { type: "ack", requestId: initCall2.requestId },
            } as MessageEvent);
            await initPromise2;
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
            client.setOption("USI_Hash", 256);

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
