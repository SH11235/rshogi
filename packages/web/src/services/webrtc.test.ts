import type { GameMessage } from "@/types/online";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WebRTCConnection, WebRTCError } from "./webrtc";

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

// グローバルwindowオブジェクトにモックを設定
const mockWindow = {
    wasmModule: mockWasmModule,
    addEventListener: vi.fn(),
    setTimeout: vi.fn((callback: () => void) => {
        callback();
        return 1;
    }),
    clearTimeout: vi.fn(),
    dispatchEvent: vi.fn(),
};

// @ts-expect-error グローバルwindowモックのためanyを使用
global.window = mockWindow;

describe("WebRTCConnection", () => {
    let connection: WebRTCConnection;

    beforeEach(() => {
        vi.clearAllMocks();
        mockWasmModule.create_webrtc_peer.mockResolvedValue(mockPeer);
        // WASMモジュールを確実に設定
        mockWindow.wasmModule = mockWasmModule;
        connection = new WebRTCConnection();
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
            // 接続状態を手動で設定
            // @ts-expect-error プライベートプロパティへのアクセス
            connection.isConnected = true;

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
            const addEventListenerMock = window.addEventListener as jest.Mock;
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
            messageListener?.({
                detail: JSON.stringify(testMessage),
            });

            expect(messageHandler).toHaveBeenCalledWith(testMessage);
        });

        it("接続状態変更ハンドラーが登録・実行される", () => {
            const stateHandler = vi.fn();
            connection.onConnectionStateChange(stateHandler);

            // イベントリスナーを取得
            const addEventListenerMock = window.addEventListener as jest.Mock;
            const eventListeners = addEventListenerMock.mock.calls;
            const stateListener = eventListeners.find(
                (call: unknown[]) => call[0] === "webrtc-connection-state",
            )?.[1] as ((event: Event) => void) | undefined;

            // 切断イベントをシミュレート
            stateListener?.({
                detail: "Disconnected",
            });

            expect(stateHandler).toHaveBeenCalledWith("disconnected");
        });
    });

    describe("再接続機能", () => {
        it("切断時に自動再接続を試行する", async () => {
            await connection.createHost();

            // イベントリスナーを取得
            const addEventListenerMock = window.addEventListener as jest.Mock;
            const eventListeners = addEventListenerMock.mock.calls;
            const stateListener = eventListeners.find(
                (call: unknown[]) => call[0] === "webrtc-connection-state",
            )?.[1] as ((event: Event) => void) | undefined;

            // 切断イベントをシミュレート
            stateListener?.({
                detail: "Disconnected",
            });

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

            // 再接続試行回数を最大値に設定
            // @ts-expect-error プライベートプロパティへのアクセス
            connection.reconnectAttempts = 5;

            // イベントリスナーを取得
            const addEventListenerMock = window.addEventListener as jest.Mock;
            const eventListeners = addEventListenerMock.mock.calls;
            const stateListener = eventListeners.find(
                (call: unknown[]) => call[0] === "webrtc-connection-state",
            )?.[1] as ((event: Event) => void) | undefined;

            // 切断イベントをシミュレート
            stateListener?.({
                detail: "Disconnected",
            });

            expect(errorHandler).toHaveBeenCalledWith(
                expect.objectContaining({
                    code: "CONNECTION_LOST",
                }),
            );
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
