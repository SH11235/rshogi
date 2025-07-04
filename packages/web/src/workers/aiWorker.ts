import { AIEngine, OpeningBookLoader, generateMainOpenings } from "shogi-core";
import type {
    AIResponse,
    AIWorkerMessage,
    ErrorResponse,
    MoveCalculatedResponse,
    PositionEvaluatedResponse,
} from "../types/ai";

// WebWorker context
const ctx: Worker = self as unknown as Worker;

// AI engine instance
let engine: AIEngine | null = null;
let openingBookLoader: OpeningBookLoader | null = null;

// Message handler
ctx.addEventListener("message", async (event: MessageEvent<AIWorkerMessage>) => {
    const message = event.data;

    try {
        switch (message.type) {
            case "initialize": {
                // Initialize AI engine with specified difficulty
                engine = new AIEngine(message.difficulty);

                // Load opening book data
                console.log(`[Worker] AI難易度: ${message.difficulty}`);
                if (message.difficulty === "beginner") {
                    // ビギナー向けの豊富な定跡データを使用
                    try {
                        const response = await fetch("/data/beginner-openings.json");
                        if (response.ok) {
                            const data = await response.json();
                            engine.loadOpeningBook(data.entries);
                            console.log(
                                `📚 ビギナー定跡: ${data.entries.length} エントリ読み込み完了`,
                            );
                        } else {
                            // フォールバック：基本定跡を使用
                            console.log(
                                "[Worker] ビギナー定跡ファイルが見つかりません。基本定跡を使用します。",
                            );
                            const openingData = generateMainOpenings();
                            engine.loadOpeningBook(openingData);
                            console.log(`[Worker] 基本定跡: ${openingData.length} エントリ生成`);
                        }
                    } catch (error) {
                        console.error("ビギナー定跡読み込みエラー:", error);
                        const openingData = generateMainOpenings();
                        engine.loadOpeningBook(openingData);
                        console.log(
                            `[Worker] エラー時フォールバック: ${openingData.length} エントリ生成`,
                        );
                    }
                } else {
                    // 中級以上は大容量定跡データベースを使用
                    try {
                        // 定跡データのベースURL（ビルド時に置き換え）
                        const baseUrl = "/data/openings";
                        openingBookLoader = new OpeningBookLoader(baseUrl);

                        // 初期ロード（最初の5ファイル）
                        await openingBookLoader.initialize({
                            preloadCount: 5,
                            onProgress: (progress) => {
                                // 進捗をメインスレッドに通知
                                const progressResponse: AIResponse = {
                                    type: "opening_book_progress",
                                    requestId: message.requestId,
                                    progress,
                                };
                                ctx.postMessage(progressResponse);
                            },
                        });

                        // 定跡データをエンジンに設定
                        const openingBook = openingBookLoader.getOpeningBook();
                        engine.setOpeningBook(openingBook);
                        console.log(
                            `[Worker] 初期定跡データ: ${openingBook.size()} エントリ読み込み完了 (残りはバックグラウンドで読み込み中)`,
                        );
                    } catch (error) {
                        console.error("定跡データベース読み込みエラー:", error);
                        // フォールバック：基本定跡を使用
                        console.log("[Worker] 大容量定跡読み込み失敗。基本定跡を使用します。");
                        const openingData = generateMainOpenings();
                        engine.loadOpeningBook(openingData);
                        console.log(`[Worker] 基本定跡: ${openingData.length} エントリ生成`);
                    }
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
