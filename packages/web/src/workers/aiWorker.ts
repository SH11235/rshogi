import init from "@/wasm/shogi_core";
import { AIEngine } from "shogi-core";
import { WasmOpeningBookLoader } from "../services/wasmOpeningBookLoader";
import type {
    AIResponse,
    AIWorkerMessage,
    ErrorResponse,
    MoveCalculatedResponse,
    PositionEvaluatedResponse,
} from "../types/ai";

// WebWorker環境でのWASMモジュール可用性をチェック
console.log("[Worker Debug] Worker environment loaded");
console.log(
    "[Worker Debug] Is WebWorker environment:",
    typeof (self as unknown as { importScripts?: unknown }).importScripts !== "undefined",
);
console.log("[Worker Debug] WebAssembly support:", typeof WebAssembly !== "undefined");

// WebWorker context
const ctx: Worker = self as unknown as Worker;

// AI engine instance
let engine: AIEngine | null = null;

// WASM opening book loader（WebWorker環境での初期化）
let openingBookLoader: WasmOpeningBookLoader | null = null;
let wasmInitialized = false;

// WebWorker環境でのWASMモジュール初期化
async function initializeWasm(): Promise<void> {
    if (wasmInitialized) return;

    try {
        console.log("[Worker Debug] Initializing WASM module in Worker");
        await init(); // WASMモジュールを初期化
        wasmInitialized = true;
        console.log("[Worker Debug] WASM module initialized successfully");
    } catch (error) {
        console.error("[Worker Debug] Failed to initialize WASM module:", error);
        throw new Error(
            `WASM initialization failed: ${error instanceof Error ? error.message : "Unknown error"}`,
        );
    }
}

// WebWorker環境でのWASMローダー初期化
async function initializeWasmLoader(): Promise<WasmOpeningBookLoader> {
    // WASMモジュールが初期化されていない場合は初期化
    await initializeWasm();

    if (!openingBookLoader) {
        console.log("[Worker Debug] Creating WasmOpeningBookLoader in Worker");
        openingBookLoader = new WasmOpeningBookLoader();
    }
    return openingBookLoader;
}

// Message handler
ctx.addEventListener("message", async (event: MessageEvent<AIWorkerMessage>) => {
    const message = event.data;

    try {
        switch (message.type) {
            case "initialize": {
                // WebWorker環境でWASMローダーを初期化
                console.log(`[Worker] AI難易度: ${message.difficulty}`);

                try {
                    const loader = await initializeWasmLoader();
                    console.log("[Worker Debug] WASM loader initialized successfully");

                    // Initialize AI engine with specified difficulty
                    engine = new AIEngine(message.difficulty, loader);

                    // 定跡データを読み込む
                    try {
                        await engine.loadOpeningBook();
                        console.log("[Worker] 定跡データを読み込みました");
                    } catch (error) {
                        console.warn("[Worker] 定跡データの読み込みに失敗しました:", error);
                        // エラーが発生しても続行（フォールバックが使用される）
                    }
                } catch (error) {
                    console.error("[Worker Debug] Failed to initialize WASM loader:", error);
                    throw new Error(
                        `Failed to initialize WASM loader: ${error instanceof Error ? error.message : "Unknown error"}`,
                    );
                }

                const response: AIResponse = {
                    type: "initialized",
                    requestId: message.requestId,
                };
                ctx.postMessage(response);
                break;
            }

            case "calculate_move": {
                if (!engine) {
                    throw new Error("AI engine not initialized");
                }

                // Calculate best move
                console.log(
                    "[Worker] calculateBestMove called with moveHistory:",
                    message.moveHistory?.length || 0,
                );
                const move = await engine.calculateBestMove(
                    message.board,
                    message.hands,
                    message.currentPlayer,
                    message.moveHistory,
                );

                const response: MoveCalculatedResponse = {
                    type: "move_calculated",
                    requestId: message.requestId,
                    move,
                    evaluation: engine.getLastEvaluation(),
                };
                ctx.postMessage(response);
                break;
            }

            case "evaluate_position": {
                if (!engine) {
                    throw new Error("AI engine not initialized");
                }

                // Evaluate current position
                const { score, depth, pv, nodes, time } = engine.evaluatePosition(
                    message.board,
                    message.hands,
                    message.currentPlayer,
                );

                const response: PositionEvaluatedResponse = {
                    type: "position_evaluated",
                    requestId: message.requestId,
                    evaluation: {
                        score,
                        depth: depth || 0,
                        pv: pv || [],
                        nodes: nodes || 0,
                        time: time || 0,
                    },
                };
                ctx.postMessage(response);
                break;
            }

            case "stop": {
                if (engine) {
                    engine.stop();
                }
                const response: AIResponse = {
                    type: "stopped",
                    requestId: message.requestId,
                };
                ctx.postMessage(response);
                break;
            }

            case "set_difficulty": {
                if (!engine) {
                    throw new Error("AI engine not initialized");
                }
                engine.setDifficulty(message.difficulty);

                // 難易度変更時に定跡データも再読み込み
                try {
                    // WASMローダーの再初期化が必要な場合
                    await initializeWasmLoader();
                    await engine.loadOpeningBook();
                    console.log(
                        `[Worker] 定跡データを再読み込みしました（難易度: ${message.difficulty}）`,
                    );
                } catch (error) {
                    console.warn("[Worker] 定跡データの再読み込みに失敗しました:", error);
                }

                const response: AIResponse = {
                    type: "difficulty_set",
                    requestId: message.requestId,
                };
                ctx.postMessage(response);
                break;
            }

            default: {
                // Make TypeScript check exhaustiveness
                return;
            }
        }
    } catch (error) {
        const errorResponse: ErrorResponse = {
            type: "error",
            requestId: message.requestId,
            error: error instanceof Error ? error.message : "Unknown error",
        };
        ctx.postMessage(errorResponse);
    }
});

// Notify that worker is ready
ctx.postMessage({ type: "ready" });
