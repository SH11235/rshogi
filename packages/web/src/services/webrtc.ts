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

// WebRTC接続を管理するクラス
export class WebRTCConnection {
    private peer: SimpleWebRTCPeer | null = null;
    private wasmModule: WasmModule | null = null;
    private isHost = false;
    private isConnected = false;
    private peerId = "";
    private messageHandlers: Array<(message: GameMessage) => void> = [];
    private connectionStateHandlers: Array<(state: RTCPeerConnectionState) => void> = [];

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
            }
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
        await this.loadWasmModule();
        if (!this.wasmModule) throw new Error("WASM module not loaded");

        this.isHost = true;
        this.peer = await this.wasmModule.create_webrtc_peer(true);
        this.peerId = this.peer.get_peer_id();

        const offer = await this.peer.create_offer();
        return offer;
    }

    async joinAsGuest(offer: string): Promise<string> {
        await this.loadWasmModule();
        if (!this.wasmModule) throw new Error("WASM module not loaded");

        this.isHost = false;
        this.peer = await this.wasmModule.create_webrtc_peer(false);
        this.peerId = this.peer.get_peer_id();

        const answer = await this.peer.handle_offer(offer);
        return answer;
    }

    async acceptAnswer(answer: string): Promise<void> {
        if (!this.peer) throw new Error("No peer connection");
        await this.peer.handle_answer(answer);
    }

    sendMessage(message: GameMessage): void {
        if (!this.peer || !this.isConnected) {
            console.error("Cannot send message: not connected");
            return;
        }

        const messageStr = JSON.stringify(message);
        this.peer.send_message(messageStr);
    }

    onMessage(handler: (message: GameMessage) => void): void {
        this.messageHandlers.push(handler);
    }

    onConnectionStateChange(handler: (state: RTCPeerConnectionState) => void): void {
        this.connectionStateHandlers.push(handler);
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

    disconnect(): void {
        this.peer = null;
        this.isConnected = false;
        this.peerId = "";
        this.notifyConnectionState("closed");
    }

    getConnectionInfo() {
        return {
            isConnected: this.isConnected,
            isHost: this.isHost,
            peerId: this.peerId,
        };
    }
}
