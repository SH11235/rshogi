import type { GameMessage } from "@/types/online";

// ===========================================
// 型定義
// ===========================================

// WASMモジュールの型定義
interface SimpleWebRTCPeer {
    get_peer_id(): string;
    create_offer(): Promise<string>;
    handle_offer(offer: string): Promise<string>;
    handle_answer(answer: string): Promise<void>;
    send_message(message: string): void;
}

interface WasmModule {
    create_webrtc_peer(isHost: boolean): Promise<SimpleWebRTCPeer>;
}

// エラータイプの定義
export type WebRTCErrorCode =
    | "WASM_NOT_LOADED"
    | "CREATE_HOST_FAILED"
    | "JOIN_FAILED"
    | "NO_PEER"
    | "ACCEPT_ANSWER_FAILED"
    | "NOT_CONNECTED"
    | "SEND_FAILED"
    | "CONNECTION_LOST"
    | "RECONNECT_FAILED"
    | "WEBRTC_ERROR"
    | "ICE_FAILED"
    | "SIGNALING_FAILED"
    | "DATA_CHANNEL_ERROR"
    | "TIMEOUT";

export class WebRTCError extends Error {
    constructor(
        public code: WebRTCErrorCode,
        message: string,
        public details?: unknown,
    ) {
        super(message);
        this.name = "WebRTCError";
    }

    getUserMessage(): string {
        switch (this.code) {
            case "WASM_NOT_LOADED":
                return "通信モジュールの読み込みに失敗しました。ページを再読み込みしてください。";
            case "CREATE_HOST_FAILED":
                return "部屋の作成に失敗しました。ネットワーク接続を確認してください。";
            case "JOIN_FAILED":
                return "部屋への参加に失敗しました。接続情報が正しいか確認してください。";
            case "NO_PEER":
                return "接続が確立されていません。";
            case "ACCEPT_ANSWER_FAILED":
                return "接続の確立に失敗しました。接続情報が正しいか確認してください。";
            case "NOT_CONNECTED":
                return "相手との接続が切断されています。";
            case "SEND_FAILED":
                return "メッセージの送信に失敗しました。";
            case "CONNECTION_LOST":
                return "相手との接続が失われました。再接続を試みています...";
            case "RECONNECT_FAILED":
                return "再接続に失敗しました。新しい接続を作成してください。";
            case "ICE_FAILED":
                return "ネットワーク経路の確立に失敗しました。ファイアウォール設定を確認してください。";
            case "SIGNALING_FAILED":
                return "接続情報の交換に失敗しました。";
            case "DATA_CHANNEL_ERROR":
                return "データ通信チャネルでエラーが発生しました。";
            case "TIMEOUT":
                return "接続がタイムアウトしました。";
            default:
                return this.message;
        }
    }
}

// 再接続の設定
interface ReconnectConfig {
    maxAttempts: number;
    retryDelay: number;
    backoffMultiplier: number;
}

// 接続品質メトリクス
export interface ConnectionQuality {
    latency: number; // ms
    packetsLost: number;
    jitter: number; // ms
    lastUpdate: number;
}

// 接続進捗状態
export type ConnectionProgress =
    | "idle"
    | "creating_offer"
    | "waiting_answer"
    | "establishing"
    | "ice_gathering"
    | "ice_checking"
    | "connected"
    | "failed";

// WebRTC接続インターフェース
export interface WebRTCConnection {
    createHost(): Promise<string>;
    joinAsGuest(offer: string): Promise<string>;
    acceptAnswer(answer: string): Promise<void>;
    sendMessage(message: GameMessage): void;
    onMessage(handler: (message: GameMessage) => void): void;
    onConnectionStateChange(handler: (state: RTCPeerConnectionState) => void): void;
    onError(handler: (error: WebRTCError) => void): void;
    onQualityChange(handler: (quality: ConnectionQuality) => void): void;
    onProgressChange(handler: (progress: ConnectionProgress) => void): void;
    disconnect(): void;
    reconnect(): Promise<void>;
    getConnectionInfo(): {
        isConnected: boolean;
        isHost: boolean;
        peerId: string;
        reconnectAttempts: number;
        canReconnect: boolean;
    };
    getConnectionQuality(): ConnectionQuality;
}

// ===========================================
// ユーティリティ関数
// ===========================================

// WASMモジュールのロード
const loadWasmModule = async (): Promise<WasmModule> => {
    try {
        // グローバルスコープからWASMモジュールにアクセス
        const wasmModule = (window as Window & { wasmModule?: unknown }).wasmModule;
        if (!wasmModule) {
            throw new Error("WASM module not found in global scope");
        }
        return wasmModule as WasmModule;
    } catch (error) {
        console.error("Failed to load WASM module:", error);
        throw new Error("WASM module loading failed");
    }
};

// ===========================================
// WebRTC接続ファクトリー
// ===========================================

/**
 * WebRTC接続インスタンスを作成する
 * クロージャーを使用して状態を管理
 */
export const createWebRTCConnection = (): WebRTCConnection => {
    // インスタンス固有の状態
    let peer: SimpleWebRTCPeer | null = null;
    let wasmModule: WasmModule | null = null;
    let isHost = false;
    let isConnected = false;
    let peerId = "";
    const messageHandlers: Array<(message: GameMessage) => void> = [];
    const connectionStateHandlers: Array<(state: RTCPeerConnectionState) => void> = [];
    const errorHandlers: Array<(error: WebRTCError) => void> = [];
    const qualityHandlers: Array<(quality: ConnectionQuality) => void> = [];
    const progressHandlers: Array<(progress: ConnectionProgress) => void> = [];

    // 接続品質の追跡
    let connectionQuality: ConnectionQuality = {
        latency: 0,
        packetsLost: 0,
        jitter: 0,
        lastUpdate: Date.now(),
    };
    let lastPingTime = 0;
    let pingInterval: number | null = null;
    let statsInterval: number | null = null;

    // 再接続用の状態
    let reconnectAttempts = 0;
    let reconnectTimer: number | null = null;
    let lastOffer = "";
    const reconnectConfig: ReconnectConfig = {
        maxAttempts: 5,
        retryDelay: 2000,
        backoffMultiplier: 1.5,
    };

    // イベントリスナーの保持（クリーンアップ用）
    const eventListeners: Array<{ event: string; handler: EventListener }> = [];

    // 通知関数
    const notifyConnectionState = (state: RTCPeerConnectionState): void => {
        for (const handler of connectionStateHandlers) {
            handler(state);
        }
    };

    const notifyError = (error: WebRTCError): void => {
        for (const handler of errorHandlers) {
            handler(error);
        }
    };

    const notifyQuality = (quality: ConnectionQuality): void => {
        for (const handler of qualityHandlers) {
            handler(quality);
        }
    };

    const notifyProgress = (progress: ConnectionProgress): void => {
        for (const handler of progressHandlers) {
            handler(progress);
        }
    };

    const handleMessage = (message: GameMessage): void => {
        for (const handler of messageHandlers) {
            handler(message);
        }
    };

    // Ping/Pong処理
    const handlePing = (message: GameMessage): void => {
        const pongMessage = {
            type: "pong" as never,
            timestamp: message.timestamp,
            data: null,
            playerId: peerId,
        };
        try {
            if (peer) {
                peer.send_message(JSON.stringify(pongMessage));
            }
        } catch (error) {
            console.error("Failed to send pong:", error);
        }
    };

    const handlePong = (message: GameMessage): void => {
        const latency = Date.now() - message.timestamp;
        connectionQuality = {
            ...connectionQuality,
            latency,
            lastUpdate: Date.now(),
        };
        notifyQuality(connectionQuality);
    };

    // 品質監視
    const startQualityMonitoring = (): void => {
        // Pingメッセージを定期的に送信
        pingInterval = window.setInterval(() => {
            if (isConnected && peer) {
                const pingMessage = {
                    type: "ping" as never,
                    timestamp: Date.now(),
                    data: null,
                    playerId: peerId,
                };
                try {
                    peer.send_message(JSON.stringify(pingMessage));
                    lastPingTime = Date.now();
                } catch (error) {
                    console.error("Failed to send ping:", error);
                }
            }
        }, 5000);

        // WebRTC統計情報の取得
        statsInterval = window.setInterval(async () => {
            if (isConnected && peer) {
                await updateConnectionStats();
            }
        }, 2000);
    };

    const stopQualityMonitoring = (): void => {
        if (pingInterval) {
            clearInterval(pingInterval);
            pingInterval = null;
        }
        if (statsInterval) {
            clearInterval(statsInterval);
            statsInterval = null;
        }
    };

    const updateConnectionStats = async (): Promise<void> => {
        // pingタイムアウトのチェック
        if (lastPingTime > 0 && Date.now() - lastPingTime > 20000) {
            console.warn("Ping timeout detected");
            connectionQuality = {
                ...connectionQuality,
                latency: 999,
            };
        }
        notifyQuality(connectionQuality);
    };

    // 状態同期のリクエスト
    const requestStateSync = (): void => {
        const syncRequest: GameMessage = {
            type: "state_sync_request",
            data: null,
            timestamp: Date.now(),
            playerId: peerId,
        };
        sendMessage(syncRequest);
        console.log("Requesting state sync after reconnection");
    };

    // 再接続の実行
    const attemptReconnect = async (): Promise<void> => {
        try {
            console.log(`Attempting reconnect (attempt ${reconnectAttempts})`);

            if (!wasmModule) {
                wasmModule = await loadWasmModule();
            }

            if (isHost && lastOffer) {
                // ホストの場合は新しいオファーを生成
                if (!wasmModule) {
                    throw new WebRTCError(
                        "WASM_NOT_LOADED",
                        "WASMモジュールがロードされていません",
                    );
                }
                peer = await wasmModule.create_webrtc_peer(true);
                peerId = peer.get_peer_id();
                const newOffer = await peer.create_offer();
                lastOffer = newOffer;

                // 再接続のためのイベントを発火
                window.dispatchEvent(
                    new CustomEvent("webrtc-reconnect-offer", {
                        detail: newOffer,
                    }),
                );
            } else if (!isHost && lastOffer) {
                // ゲストの場合は保存されたオファーで再接続
                if (!wasmModule) {
                    throw new WebRTCError(
                        "WASM_NOT_LOADED",
                        "WASMモジュールがロードされていません",
                    );
                }
                peer = await wasmModule.create_webrtc_peer(false);
                peerId = peer.get_peer_id();
                const newAnswer = await peer.handle_offer(lastOffer);

                // 再接続のためのイベントを発火
                window.dispatchEvent(
                    new CustomEvent("webrtc-reconnect-answer", {
                        detail: newAnswer,
                    }),
                );
            }

            console.log("Reconnect attempt initiated");
        } catch (error) {
            console.error("Reconnect attempt failed:", error);

            if (reconnectAttempts < reconnectConfig.maxAttempts) {
                scheduleReconnect();
            } else {
                notifyError(new WebRTCError("RECONNECT_FAILED", "再接続に失敗しました"));
            }
        }
    };

    // 再接続のスケジューリング
    const scheduleReconnect = (): void => {
        if (reconnectTimer) return;

        reconnectAttempts++;
        const delay =
            reconnectConfig.retryDelay *
            reconnectConfig.backoffMultiplier ** (reconnectAttempts - 1);

        console.log(`Scheduling reconnect attempt ${reconnectAttempts} in ${delay}ms`);

        reconnectTimer = window.setTimeout(() => {
            reconnectTimer = null;
            attemptReconnect();
        }, delay);
    };

    // イベントリスナーの設定
    const setupEventListeners = (): void => {
        // WebRTCチャンネル開通
        const channelOpenHandler = () => {
            console.log("WebRTC channel opened");
            isConnected = true;
            notifyConnectionState("connected");
            notifyProgress("connected");
            startQualityMonitoring();
        };
        window.addEventListener("webrtc-channel-open", channelOpenHandler);
        eventListeners.push({ event: "webrtc-channel-open", handler: channelOpenHandler });

        // メッセージ受信
        const messageHandler = (event: Event) => {
            const customEvent = event as CustomEvent;
            const messageStr = customEvent.detail;
            try {
                const message: GameMessage = JSON.parse(messageStr);
                if (message.type === ("ping" as never)) {
                    handlePing(message);
                } else if (message.type === ("pong" as never)) {
                    handlePong(message);
                } else {
                    handleMessage(message);
                }
            } catch (error) {
                console.error("Failed to parse message:", error);
            }
        };
        window.addEventListener("webrtc-message", messageHandler);
        eventListeners.push({ event: "webrtc-message", handler: messageHandler });

        // 接続状態変更
        const connectionStateHandler = (event: Event) => {
            const customEvent = event as CustomEvent;
            const state = customEvent.detail;
            console.log("Connection state:", state);

            if (state === "Disconnected" || state === "Failed" || state === "Closed") {
                isConnected = false;
                notifyConnectionState("disconnected");
                stopQualityMonitoring();
                notifyProgress("failed");

                // 自動再接続を試行
                if (reconnectAttempts < reconnectConfig.maxAttempts) {
                    scheduleReconnect();
                } else {
                    notifyError(new WebRTCError("CONNECTION_LOST", "接続が失われました"));
                }
            } else if (state === "Checking") {
                notifyProgress("ice_checking");
            } else if (state === "Connected") {
                notifyProgress("connected");
                // 再接続後の状態同期をリクエスト
                if (reconnectAttempts > 0) {
                    requestStateSync();
                }
            }
        };
        window.addEventListener("webrtc-connection-state", connectionStateHandler);
        eventListeners.push({ event: "webrtc-connection-state", handler: connectionStateHandler });

        // エラーイベント
        const errorHandler = (event: Event) => {
            const customEvent = event as CustomEvent;
            const errorMessage = customEvent.detail;
            console.error("WebRTC error:", errorMessage);
            notifyError(new WebRTCError("WEBRTC_ERROR", errorMessage));
        };
        window.addEventListener("webrtc-error", errorHandler);
        eventListeners.push({ event: "webrtc-error", handler: errorHandler });
    };

    // Public API実装
    const createHost = async (): Promise<string> => {
        try {
            wasmModule = await loadWasmModule();
            if (!wasmModule) {
                throw new WebRTCError("WASM_NOT_LOADED", "WASMモジュールの読み込みに失敗しました");
            }

            notifyProgress("creating_offer");
            isHost = true;
            peer = await wasmModule.create_webrtc_peer(true);
            peerId = peer.get_peer_id();

            notifyProgress("ice_gathering");
            const offer = await peer.create_offer();
            lastOffer = offer;
            reconnectAttempts = 0;
            notifyProgress("waiting_answer");
            return offer;
        } catch (error) {
            const webrtcError =
                error instanceof WebRTCError
                    ? error
                    : new WebRTCError("CREATE_HOST_FAILED", `ホストの作成に失敗しました: ${error}`);
            notifyError(webrtcError);
            throw webrtcError;
        }
    };

    const joinAsGuest = async (offer: string): Promise<string> => {
        try {
            wasmModule = await loadWasmModule();
            if (!wasmModule) {
                throw new WebRTCError("WASM_NOT_LOADED", "WASMモジュールの読み込みに失敗しました");
            }

            notifyProgress("establishing");
            isHost = false;
            peer = await wasmModule.create_webrtc_peer(false);
            peerId = peer.get_peer_id();

            notifyProgress("ice_gathering");
            const answer = await peer.handle_offer(offer);
            lastOffer = offer;
            reconnectAttempts = 0;
            notifyProgress("ice_checking");
            return answer;
        } catch (error) {
            const webrtcError =
                error instanceof WebRTCError
                    ? error
                    : new WebRTCError("JOIN_FAILED", `参加に失敗しました: ${error}`);
            notifyError(webrtcError);
            throw webrtcError;
        }
    };

    const acceptAnswer = async (answer: string): Promise<void> => {
        try {
            if (!peer) throw new WebRTCError("NO_PEER", "ピア接続が存在しません");
            notifyProgress("establishing");
            await peer.handle_answer(answer);
            notifyProgress("ice_checking");
        } catch (error) {
            const webrtcError =
                error instanceof WebRTCError
                    ? error
                    : new WebRTCError(
                          "ACCEPT_ANSWER_FAILED",
                          `アンサーの受け入れに失敗しました: ${error}`,
                      );
            notifyError(webrtcError);
            throw webrtcError;
        }
    };

    const sendMessage = (message: GameMessage): void => {
        try {
            if (!peer || !isConnected) {
                throw new WebRTCError("NOT_CONNECTED", "接続されていません");
            }

            const messageStr = JSON.stringify(message);
            peer.send_message(messageStr);
        } catch (error) {
            const webrtcError =
                error instanceof WebRTCError
                    ? error
                    : new WebRTCError("SEND_FAILED", `メッセージ送信に失敗しました: ${error}`);
            notifyError(webrtcError);
            console.error("Send message error:", webrtcError);
        }
    };

    const disconnect = (): void => {
        // 再接続タイマーをクリア
        if (reconnectTimer) {
            clearTimeout(reconnectTimer);
            reconnectTimer = null;
        }

        stopQualityMonitoring();

        // イベントリスナーの削除
        for (const { event, handler } of eventListeners) {
            window.removeEventListener(event, handler);
        }
        eventListeners.length = 0;

        peer = null;
        isConnected = false;
        peerId = "";
        reconnectAttempts = 0;
        notifyConnectionState("closed");
        notifyProgress("idle");
    };

    const reconnect = async (): Promise<void> => {
        reconnectAttempts = 0;
        await attemptReconnect();
    };

    const getConnectionInfo = () => ({
        isConnected,
        isHost,
        peerId,
        reconnectAttempts,
        canReconnect: reconnectAttempts < reconnectConfig.maxAttempts,
    });

    const getConnectionQuality = (): ConnectionQuality => ({ ...connectionQuality });

    // イベントリスナーの初期設定
    setupEventListeners();

    // Public API
    return {
        createHost,
        joinAsGuest,
        acceptAnswer,
        sendMessage,
        onMessage: (handler: (message: GameMessage) => void) => {
            messageHandlers.push(handler);
        },
        onConnectionStateChange: (handler: (state: RTCPeerConnectionState) => void) => {
            connectionStateHandlers.push(handler);
        },
        onError: (handler: (error: WebRTCError) => void) => {
            errorHandlers.push(handler);
        },
        onQualityChange: (handler: (quality: ConnectionQuality) => void) => {
            qualityHandlers.push(handler);
        },
        onProgressChange: (handler: (progress: ConnectionProgress) => void) => {
            progressHandlers.push(handler);
        },
        disconnect,
        reconnect,
        getConnectionInfo,
        getConnectionQuality,
    };
};
