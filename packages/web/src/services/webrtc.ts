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
export class WebRTCError extends Error {
    constructor(
        public code: string,
        message: string,
    ) {
        super(message);
        this.name = "WebRTCError";
    }
}

// 再接続の設定
interface ReconnectConfig {
    maxAttempts: number;
    retryDelay: number;
    backoffMultiplier: number;
}

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
        });

        window.addEventListener("webrtc-message", (event: Event) => {
            const customEvent = event as CustomEvent;
            const messageStr = customEvent.detail;
            try {
                const message: GameMessage = JSON.parse(messageStr);
                this.handleMessage(message);
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

                // 自動再接続を試行
                if (this.reconnectAttempts < this.reconnectConfig.maxAttempts) {
                    this.scheduleReconnect();
                } else {
                    this.notifyError(new WebRTCError("CONNECTION_LOST", "接続が失われました"));
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

            this.isHost = true;
            this.peer = await this.wasmModule.create_webrtc_peer(true);
            this.peerId = this.peer.get_peer_id();

            const offer = await this.peer.create_offer();
            this.lastOffer = offer; // 再接続用に保存
            this.reconnectAttempts = 0; // リセット
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

            this.isHost = false;
            this.peer = await this.wasmModule.create_webrtc_peer(false);
            this.peerId = this.peer.get_peer_id();

            const answer = await this.peer.handle_offer(offer);
            this.lastOffer = offer; // 再接続用に保存
            this.reconnectAttempts = 0; // リセット
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
            await this.peer.handle_answer(answer);
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

    disconnect(): void {
        // 再接続タイマーをクリア
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }

        this.peer = null;
        this.isConnected = false;
        this.peerId = "";
        this.reconnectAttempts = 0;
        this.notifyConnectionState("closed");
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
}
