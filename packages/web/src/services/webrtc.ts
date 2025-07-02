import type { GameMessage } from "@/types/online";

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

// WebRTC接続を管理するクラス
export class WebRTCConnection {
    private peer: SimpleWebRTCPeer | null = null;
    private wasmModule: WasmModule | null = null;
    private isHost = false;
    private isConnected = false;
    private peerId = "";
    private messageHandlers: Array<(message: GameMessage) => void> = [];
    private connectionStateHandlers: Array<(state: RTCPeerConnectionState) => void> = [];
    private errorHandlers: Array<(error: WebRTCError) => void> = [];
    private qualityHandlers: Array<(quality: ConnectionQuality) => void> = [];
    private progressHandlers: Array<(progress: ConnectionProgress) => void> = [];

    // 接続品質の追跡
    private connectionQuality: ConnectionQuality = {
        latency: 0,
        packetsLost: 0,
        jitter: 0,
        lastUpdate: Date.now(),
    };
    private lastPingTime = 0; // ping/pongのタイムアウト検出に使用
    private pingInterval: number | null = null;
    private statsInterval: number | null = null;

    // 再接続用の状態
    private reconnectAttempts = 0;
    private reconnectTimer: number | null = null;
    private lastOffer = "";
    private reconnectConfig: ReconnectConfig = {
        maxAttempts: 5,
        retryDelay: 2000,
        backoffMultiplier: 1.5,
    };

    constructor() {
        this.setupEventListeners();
    }

    private setupEventListeners(): void {
        // WebRTCイベントのリスナー設定
        window.addEventListener("webrtc-channel-open", () => {
            console.log("WebRTC channel opened");
            this.isConnected = true;
            this.notifyConnectionState("connected");
            this.notifyProgress("connected");
            this.startQualityMonitoring();
        });

        window.addEventListener("webrtc-message", (event: Event) => {
            const customEvent = event as CustomEvent;
            const messageStr = customEvent.detail;
            try {
                const message: GameMessage = JSON.parse(messageStr);
                // Pingメッセージの処理
                if (message.type === ("ping" as never)) {
                    this.handlePing(message);
                } else if (message.type === ("pong" as never)) {
                    this.handlePong(message);
                } else {
                    this.handleMessage(message);
                }
            } catch (error) {
                console.error("Failed to parse message:", error);
            }
        });

        window.addEventListener("webrtc-connection-state", (event: Event) => {
            const customEvent = event as CustomEvent;
            const state = customEvent.detail;
            console.log("Connection state:", state);

            if (state === "Disconnected" || state === "Failed" || state === "Closed") {
                this.isConnected = false;
                this.notifyConnectionState("disconnected");
                this.stopQualityMonitoring();
                this.notifyProgress("failed");

                // 自動再接続を試行
                if (this.reconnectAttempts < this.reconnectConfig.maxAttempts) {
                    this.scheduleReconnect();
                } else {
                    this.notifyError(new WebRTCError("CONNECTION_LOST", "接続が失われました"));
                }
            } else if (state === "Checking") {
                this.notifyProgress("ice_checking");
            } else if (state === "Connected") {
                this.notifyProgress("connected");
                // 再接続後の状態同期をリクエスト
                if (this.reconnectAttempts > 0) {
                    this.requestStateSync();
                }
            }
        });

        // エラーイベントの監視
        window.addEventListener("webrtc-error", (event: Event) => {
            const customEvent = event as CustomEvent;
            const errorMessage = customEvent.detail;
            console.error("WebRTC error:", errorMessage);
            this.notifyError(new WebRTCError("WEBRTC_ERROR", errorMessage));
        });
    }

    private startQualityMonitoring(): void {
        // Pingメッセージを定期的に送信
        this.pingInterval = window.setInterval(() => {
            if (this.isConnected && this.peer) {
                const pingMessage = {
                    type: "ping" as never,
                    timestamp: Date.now(),
                    data: null,
                    playerId: this.peerId,
                };
                try {
                    this.peer.send_message(JSON.stringify(pingMessage));
                    this.lastPingTime = Date.now(); // ping送信時刻を記録
                } catch (error) {
                    console.error("Failed to send ping:", error);
                }
            }
        }, 5000); // 5秒ごとにping

        // WebRTC統計情報の取得
        this.statsInterval = window.setInterval(async () => {
            if (this.isConnected && this.peer) {
                await this.updateConnectionStats();
            }
        }, 2000); // 2秒ごとに統計更新
    }

    private stopQualityMonitoring(): void {
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
        if (this.statsInterval) {
            clearInterval(this.statsInterval);
            this.statsInterval = null;
        }
    }

    private handlePing(message: GameMessage): void {
        // Pingを受信したらPongを返す
        const pongMessage = {
            type: "pong" as never,
            timestamp: message.timestamp,
            data: null,
            playerId: this.peerId,
        };
        try {
            if (this.peer) {
                this.peer.send_message(JSON.stringify(pongMessage));
            }
        } catch (error) {
            console.error("Failed to send pong:", error);
        }
    }

    private handlePong(message: GameMessage): void {
        // Pongを受信したらレイテンシを計算
        const latency = Date.now() - message.timestamp;
        this.connectionQuality.latency = latency;
        this.connectionQuality.lastUpdate = Date.now();
        this.notifyQuality(this.connectionQuality);
    }

    private async updateConnectionStats(): Promise<void> {
        // 将来的にWebRTC統計APIを使用して詳細な統計を取得
        // 現在は基本的な情報のみ

        // pingタイムアウトのチェック（20秒以上応答がない場合）
        if (this.lastPingTime > 0 && Date.now() - this.lastPingTime > 20000) {
            console.warn("Ping timeout detected");
            this.connectionQuality.latency = 999; // 高レイテンシとして表示
        }

        this.notifyQuality(this.connectionQuality);
    }

    async loadWasmModule(): Promise<void> {
        if (this.wasmModule) return;

        try {
            // グローバルスコープからWASMモジュールにアクセス
            // WebRTCデモと同じ方法を使用
            const wasmModule = (window as Window & { wasmModule?: unknown }).wasmModule;
            if (!wasmModule) {
                throw new Error("WASM module not found in global scope");
            }
            this.wasmModule = wasmModule as WasmModule;
        } catch (error) {
            console.error("Failed to load WASM module:", error);
            throw new Error("WASM module loading failed");
        }
    }

    async createHost(): Promise<string> {
        try {
            await this.loadWasmModule();
            if (!this.wasmModule)
                throw new WebRTCError("WASM_NOT_LOADED", "WASMモジュールの読み込みに失敗しました");

            this.notifyProgress("creating_offer");
            this.isHost = true;
            this.peer = await this.wasmModule.create_webrtc_peer(true);
            this.peerId = this.peer.get_peer_id();

            this.notifyProgress("ice_gathering");
            const offer = await this.peer.create_offer();
            this.lastOffer = offer; // 再接続用に保存
            this.reconnectAttempts = 0; // リセット
            this.notifyProgress("waiting_answer");
            return offer;
        } catch (error) {
            const webrtcError =
                error instanceof WebRTCError
                    ? error
                    : new WebRTCError("CREATE_HOST_FAILED", `ホストの作成に失敗しました: ${error}`);
            this.notifyError(webrtcError);
            throw webrtcError;
        }
    }

    async joinAsGuest(offer: string): Promise<string> {
        try {
            await this.loadWasmModule();
            if (!this.wasmModule)
                throw new WebRTCError("WASM_NOT_LOADED", "WASMモジュールの読み込みに失敗しました");

            this.notifyProgress("establishing");
            this.isHost = false;
            this.peer = await this.wasmModule.create_webrtc_peer(false);
            this.peerId = this.peer.get_peer_id();

            this.notifyProgress("ice_gathering");
            const answer = await this.peer.handle_offer(offer);
            this.lastOffer = offer; // 再接続用に保存
            this.reconnectAttempts = 0; // リセット
            this.notifyProgress("ice_checking");
            return answer;
        } catch (error) {
            const webrtcError =
                error instanceof WebRTCError
                    ? error
                    : new WebRTCError("JOIN_FAILED", `参加に失敗しました: ${error}`);
            this.notifyError(webrtcError);
            throw webrtcError;
        }
    }

    async acceptAnswer(answer: string): Promise<void> {
        try {
            if (!this.peer) throw new WebRTCError("NO_PEER", "ピア接続が存在しません");
            this.notifyProgress("establishing");
            await this.peer.handle_answer(answer);
            this.notifyProgress("ice_checking");
        } catch (error) {
            const webrtcError =
                error instanceof WebRTCError
                    ? error
                    : new WebRTCError(
                          "ACCEPT_ANSWER_FAILED",
                          `アンサーの受け入れに失敗しました: ${error}`,
                      );
            this.notifyError(webrtcError);
            throw webrtcError;
        }
    }

    sendMessage(message: GameMessage): void {
        try {
            if (!this.peer || !this.isConnected) {
                throw new WebRTCError("NOT_CONNECTED", "接続されていません");
            }

            const messageStr = JSON.stringify(message);
            this.peer.send_message(messageStr);
        } catch (error) {
            const webrtcError =
                error instanceof WebRTCError
                    ? error
                    : new WebRTCError("SEND_FAILED", `メッセージ送信に失敗しました: ${error}`);
            this.notifyError(webrtcError);
            console.error("Send message error:", webrtcError);
        }
    }

    onMessage(handler: (message: GameMessage) => void): void {
        this.messageHandlers.push(handler);
    }

    onConnectionStateChange(handler: (state: RTCPeerConnectionState) => void): void {
        this.connectionStateHandlers.push(handler);
    }

    onError(handler: (error: WebRTCError) => void): void {
        this.errorHandlers.push(handler);
    }

    onQualityChange(handler: (quality: ConnectionQuality) => void): void {
        this.qualityHandlers.push(handler);
    }

    onProgressChange(handler: (progress: ConnectionProgress) => void): void {
        this.progressHandlers.push(handler);
    }

    private handleMessage(message: GameMessage): void {
        for (const handler of this.messageHandlers) {
            handler(message);
        }
    }

    private notifyConnectionState(state: RTCPeerConnectionState): void {
        for (const handler of this.connectionStateHandlers) {
            handler(state);
        }
    }

    private notifyError(error: WebRTCError): void {
        for (const handler of this.errorHandlers) {
            handler(error);
        }
    }

    private notifyQuality(quality: ConnectionQuality): void {
        for (const handler of this.qualityHandlers) {
            handler(quality);
        }
    }

    private notifyProgress(progress: ConnectionProgress): void {
        for (const handler of this.progressHandlers) {
            handler(progress);
        }
    }

    disconnect(): void {
        // 再接続タイマーをクリア
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }

        this.stopQualityMonitoring();
        this.peer = null;
        this.isConnected = false;
        this.peerId = "";
        this.reconnectAttempts = 0;
        this.notifyConnectionState("closed");
        this.notifyProgress("idle");
    }

    // 再接続のスケジューリング
    private scheduleReconnect(): void {
        if (this.reconnectTimer) return;

        this.reconnectAttempts++;
        const delay =
            this.reconnectConfig.retryDelay *
            this.reconnectConfig.backoffMultiplier ** (this.reconnectAttempts - 1);

        console.log(`Scheduling reconnect attempt ${this.reconnectAttempts} in ${delay}ms`);

        this.reconnectTimer = window.setTimeout(() => {
            this.reconnectTimer = null;
            this.attemptReconnect();
        }, delay);
    }

    // 再接続の実行
    private async attemptReconnect(): Promise<void> {
        try {
            console.log(`Attempting reconnect (attempt ${this.reconnectAttempts})`);

            if (!this.wasmModule) {
                await this.loadWasmModule();
            }

            if (this.isHost && this.lastOffer) {
                // ホストの場合は新しいオファーを生成
                if (!this.wasmModule)
                    throw new WebRTCError(
                        "WASM_NOT_LOADED",
                        "WASMモジュールがロードされていません",
                    );
                this.peer = await this.wasmModule.create_webrtc_peer(true);
                this.peerId = this.peer.get_peer_id();
                const newOffer = await this.peer.create_offer();
                this.lastOffer = newOffer;

                // 再接続のためのイベントを発火
                window.dispatchEvent(
                    new CustomEvent("webrtc-reconnect-offer", {
                        detail: newOffer,
                    }),
                );
            } else if (!this.isHost && this.lastOffer) {
                // ゲストの場合は保存されたオファーで再接続
                if (!this.wasmModule)
                    throw new WebRTCError(
                        "WASM_NOT_LOADED",
                        "WASMモジュールがロードされていません",
                    );
                this.peer = await this.wasmModule.create_webrtc_peer(false);
                this.peerId = this.peer.get_peer_id();
                const newAnswer = await this.peer.handle_offer(this.lastOffer);

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

            if (this.reconnectAttempts < this.reconnectConfig.maxAttempts) {
                this.scheduleReconnect();
            } else {
                this.notifyError(new WebRTCError("RECONNECT_FAILED", "再接続に失敗しました"));
            }
        }
    }

    // 手動での再接続
    async reconnect(): Promise<void> {
        this.reconnectAttempts = 0;
        await this.attemptReconnect();
    }

    getConnectionInfo() {
        return {
            isConnected: this.isConnected,
            isHost: this.isHost,
            peerId: this.peerId,
            reconnectAttempts: this.reconnectAttempts,
            canReconnect: this.reconnectAttempts < this.reconnectConfig.maxAttempts,
        };
    }

    getConnectionQuality(): ConnectionQuality {
        return { ...this.connectionQuality };
    }

    // 状態同期のリクエスト
    private requestStateSync(): void {
        const syncRequest: GameMessage = {
            type: "state_sync_request",
            data: null,
            timestamp: Date.now(),
            playerId: this.peerId,
        };
        this.sendMessage(syncRequest);
        console.log("Requesting state sync after reconnection");
    }
}
