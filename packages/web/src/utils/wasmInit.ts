import init from "@/wasm/shogi_core";

/**
 * WASM初期化を管理するシングルトンクラス
 * メインスレッドでのWASM初期化を一元管理し、重複初期化を防ぐ
 */
class WasmInitializer {
    private static instance: WasmInitializer | null = null;
    private initialized = false;
    private initPromise: Promise<void> | null = null;

    private constructor() {}

    /**
     * シングルトンインスタンスを取得
     */
    static getInstance(): WasmInitializer {
        if (!WasmInitializer.instance) {
            WasmInitializer.instance = new WasmInitializer();
        }
        return WasmInitializer.instance;
    }

    /**
     * WASMモジュールが初期化済みであることを保証する
     * 初回呼び出し時に初期化を実行し、以降は即座に完了する
     */
    async ensureInitialized(): Promise<void> {
        if (this.initialized) return;

        if (this.initPromise) {
            return this.initPromise;
        }

        this.initPromise = this.performInit();
        return this.initPromise;
    }

    /**
     * 実際の初期化処理
     */
    private async performInit(): Promise<void> {
        try {
            const wasmPath = getWasmPath();
            await init(wasmPath);
            this.initialized = true;
            console.log("[WasmInitializer] WASM module initialized successfully");
        } catch (error) {
            console.error("[WasmInitializer] Failed to initialize WASM module:", error);
            // 失敗時は次回リトライできるようにリセット
            this.initPromise = null;
            throw new Error(
                `WASM initialization failed: ${error instanceof Error ? error.message : "Unknown error"}`,
            );
        }
    }

    /**
     * 初期化状態をリセット（テスト用）
     */
    resetForTesting(): void {
        this.initialized = false;
        this.initPromise = null;
    }
}

/**
 * WASMファイルのパスを取得
 * WebWorkerとメインスレッドの両方で使用可能
 */
export function getWasmPath(): string {
    return `${import.meta.env.BASE_URL}wasm/shogi_core_bg.wasm`;
}

/**
 * WASMモジュールの初期化を保証する便利関数
 * メインスレッドでの使用を想定
 */
export async function ensureWasmInitialized(): Promise<void> {
    return WasmInitializer.getInstance().ensureInitialized();
}

/**
 * WasmInitializerインスタンスを取得（高度な使用向け）
 */
export function getWasmInitializer(): WasmInitializer {
    return WasmInitializer.getInstance();
}
