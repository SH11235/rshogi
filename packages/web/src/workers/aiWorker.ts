import { AIEngine, generateMainOpenings } from "shogi-core";
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

// Message handler
ctx.addEventListener("message", async (event: MessageEvent<AIWorkerMessage>) => {
    const message = event.data;

    try {
        switch (message.type) {
            case "initialize": {
                // Initialize AI engine with specified difficulty
                engine = new AIEngine(message.difficulty);
                // Load opening book data for intermediate and above
                if (message.difficulty !== "beginner") {
                    const openingData = generateMainOpenings();
                    engine.loadOpeningBook(openingData);
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
