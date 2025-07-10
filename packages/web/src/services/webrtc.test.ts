import type { GameMessage } from "@/types/online";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type WebRTCConnection, WebRTCError, createWebRTCConnection } from "./webrtc";

// モックWASMモジュール
const mockWasmModule = {
    create_webrtc_peer: vi.fn(),
};

// モックピア
const mockPeer = {
    get_peer_id: vi.fn(() => "test-peer-id"),
    create_offer: vi.fn(() => Promise.resolve("test-offer")),
    handle_offer: vi.fn(() => Promise.resolve("test-answer")),
    handle_answer: vi.fn(() => Promise.resolve()),
    send_message: vi.fn(),
};

// イベントリスナーを保持するためのマップ
const eventListeners = new Map<string, Set<EventListener>>();

// グローバルwindowオブジェクトにモックを設定
const mockWindow = {
    wasmModule: mockWasmModule,
    addEventListener: vi.fn((event: string, handler: EventListener) => {
        if (!eventListeners.has(event)) {
            eventListeners.set(event, new Set());
        }
        eventListeners.get(event)?.add(handler);
    }),
    removeEventListener: vi.fn((event: string, handler: EventListener) => {
        eventListeners.get(event)?.delete(handler);
    }),
    setTimeout: vi.fn((callback: () => void) => {
        callback();
        return 1;
    }),
    clearTimeout: vi.fn(),
    setInterval: vi.fn(() => 1),
    clearInterval: vi.fn(),
    dispatchEvent: vi.fn((event: Event) => {
        const handlers = eventListeners.get(event.type);
        if (handlers) {
            for (const handler of handlers) {
                handler(event);
            }
        }
    }),
};

// @ts-expect-error グローバルwindowモックのためanyを使用
global.window = mockWindow;

describe("WebRTCConnection", () => {
    let connection: WebRTCConnection;

    beforeEach(() => {
        vi.clearAllMocks();
        eventListeners.clear();
        mockWasmModule.create_webrtc_peer.mockResolvedValue(mockPeer);
        // WASMモジュールを確実に設定
        mockWindow.wasmModule = mockWasmModule;
        connection = createWebRTCConnection();
    });

    afterEach(() => {
        connection.disconnect();
    });

    describe("createHost", () => {
        it("ホストとして接続を作成できる", async () => {
            const offer = await connection.createHost();

            expect(offer).toBe("test-offer");
            expect(mockWasmModule.create_webrtc_peer).toHaveBeenCalledWith(true);
            expect(connection.getConnectionInfo().isHost).toBe(true);
            expect(connection.getConnectionInfo().peerId).toBe("test-peer-id");
        });

        it("WASMモジュールが読み込まれていない場合エラーを投げる", async () => {
            // @ts-expect-error 意図的にnullを設定してエラーケースをテスト
            mockWindow.wasmModule = null;

            await expect(connection.createHost()).rejects.toThrow(WebRTCError);
        });
    });

    describe("joinAsGuest", () => {
        it("ゲストとして接続できる", async () => {
            const answer = await connection.joinAsGuest("test-offer");

            expect(answer).toBe("test-answer");
            expect(mockWasmModule.create_webrtc_peer).toHaveBeenCalledWith(false);
            expect(mockPeer.handle_offer).toHaveBeenCalledWith("test-offer");
            expect(connection.getConnectionInfo().isHost).toBe(false);
        });
    });

    describe("acceptAnswer", () => {
        it("アンサーを受け入れられる", async () => {
            await connection.createHost();
            await connection.acceptAnswer("test-answer");

            expect(mockPeer.handle_answer).toHaveBeenCalledWith("test-answer");
        });

        it("ピア接続がない場合エラーを投げる", async () => {
            await expect(connection.acceptAnswer("test-answer")).rejects.toThrow(WebRTCError);
        });
    });

    describe("sendMessage", () => {
        it("接続中にメッセージを送信できる", async () => {
            await connection.createHost();

            // 接続完了イベントを発火して接続状態にする
            const channelOpenEvent = new Event("webrtc-channel-open");
            window.dispatchEvent(channelOpenEvent);

            const message: GameMessage = {
                type: "move",
                data: { from: "77", to: "76" },
                timestamp: Date.now(),
                playerId: "test-peer-id",
            };

            connection.sendMessage(message);

            expect(mockPeer.send_message).toHaveBeenCalledWith(JSON.stringify(message));
        });

        it("未接続時はエラーハンドラーが呼ばれる", async () => {
            const errorHandler = vi.fn();
            connection.onError(errorHandler);

            const message: GameMessage = {
                type: "move",
                data: { from: "77", to: "76" },
                timestamp: Date.now(),
                playerId: "test-peer-id",
            };

            connection.sendMessage(message);

            expect(errorHandler).toHaveBeenCalledWith(
                expect.objectContaining({
                    code: "NOT_CONNECTED",
                }),
            );
        });
    });

    describe("イベントハンドリング", () => {
        it("メッセージハンドラーが登録・実行される", () => {
            const messageHandler = vi.fn();
            connection.onMessage(messageHandler);

            // イベントリスナーを取得
            const addEventListenerMock = window.addEventListener as ReturnType<typeof vi.fn>;
            const eventListeners = addEventListenerMock.mock.calls;
            const messageListener = eventListeners.find(
                (call: unknown[]) => call[0] === "webrtc-message",
            )?.[1] as ((event: Event) => void) | undefined;

            const testMessage: GameMessage = {
                type: "move",
                data: { from: "77", to: "76" },
                timestamp: Date.now(),
                playerId: "test-peer-id",
            };

            // イベントをシミュレート
            const event = new CustomEvent("webrtc-message", {
                detail: JSON.stringify(testMessage),
            });
            messageListener?.(event);

            expect(messageHandler).toHaveBeenCalledWith(testMessage);
        });

        it("接続状態変更ハンドラーが登録・実行される", () => {
            const stateHandler = vi.fn();
            connection.onConnectionStateChange(stateHandler);

            // イベントリスナーを取得
            const addEventListenerMock = window.addEventListener as ReturnType<typeof vi.fn>;
            const eventListeners = addEventListenerMock.mock.calls;
            const stateListener = eventListeners.find(
                (call: unknown[]) => call[0] === "webrtc-connection-state",
            )?.[1] as ((event: Event) => void) | undefined;

            // 切断イベントをシミュレート
            const event = new CustomEvent("webrtc-connection-state", {
                detail: "Disconnected",
            });
            stateListener?.(event);

            expect(stateHandler).toHaveBeenCalledWith("disconnected");
        });
    });

    describe("再接続機能", () => {
        it("切断時に自動再接続を試行する", async () => {
            await connection.createHost();

            // イベントリスナーを取得
            const addEventListenerMock = window.addEventListener as ReturnType<typeof vi.fn>;
            const eventListeners = addEventListenerMock.mock.calls;
            const stateListener = eventListeners.find(
                (call: unknown[]) => call[0] === "webrtc-connection-state",
            )?.[1] as ((event: Event) => void) | undefined;

            // 切断イベントをシミュレート
            const event = new CustomEvent("webrtc-connection-state", {
                detail: "Disconnected",
            });
            stateListener?.(event);

            // setTimeoutがコールバックを即座に実行するので、再接続が試行される
            expect(mockWasmModule.create_webrtc_peer).toHaveBeenCalledTimes(2);
        });

        it("手動での再接続ができる", async () => {
            await connection.createHost();
            vi.clearAllMocks();

            await connection.reconnect();

            expect(mockWasmModule.create_webrtc_peer).toHaveBeenCalledWith(true);
        });

        it("最大試行回数を超えるとエラーハンドラーが呼ばれる", async () => {
            const errorHandler = vi.fn();
            connection.onError(errorHandler);
            await connection.createHost();

            // setTimeoutモックを実際のタイマー動作に変更
            mockWindow.setTimeout = vi.fn((callback: () => void, _delay?: number) => {
                // 遅延実行を無視して即座に実行
                setTimeout(callback, 0);
                return 1;
            }) as unknown as typeof mockWindow.setTimeout;

            // 再接続を失敗させる
            mockWasmModule.create_webrtc_peer.mockRejectedValue(new Error("Connection failed"));

            // 6回の切断イベントをシミュレート（最大試行回数+1）
            for (let i = 0; i < 6; i++) {
                const event = new CustomEvent("webrtc-connection-state", {
                    detail: "Disconnected",
                });
                window.dispatchEvent(event);
                // 非同期処理を待つ
                await new Promise((resolve) => setTimeout(resolve, 10));
            }

            // エラーハンドラーが呼ばれることを確認
            expect(errorHandler).toHaveBeenCalled();
            const lastCall = errorHandler.mock.calls[errorHandler.mock.calls.length - 1];
            expect(lastCall[0]).toMatchObject({
                code: "CONNECTION_LOST",
            });
        });
    });

    describe("disconnect", () => {
        it("接続を正しく切断する", async () => {
            await connection.createHost();
            const stateHandler = vi.fn();
            connection.onConnectionStateChange(stateHandler);

            connection.disconnect();

            expect(connection.getConnectionInfo().isConnected).toBe(false);
            expect(connection.getConnectionInfo().peerId).toBe("");
            expect(stateHandler).toHaveBeenCalledWith("closed");
        });
    });
});
