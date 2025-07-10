import type { Board, Hands, Move, Player } from "shogi-core";
import type {
    AIDifficulty,
    AIPlayer,
    AIResponse,
    AIWorkerMessage,
    CalculateMoveRequest,
    EvaluatePositionRequest,
    InitializeRequest,
    PositionEvaluation,
    SetDifficultyRequest,
} from "../../types/ai";
import {
    isErrorResponse,
    isMoveCalculatedResponse,
    isPositionEvaluatedResponse,
} from "../../types/ai";

// ===========================================
// 型定義
// ===========================================

export interface AIService {
    initialize(): Promise<void>;
    calculateMove(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        moveHistory: Move[],
    ): Promise<{ move: Move; evaluation: PositionEvaluation }>;
    evaluatePosition(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
    ): Promise<PositionEvaluation>;
    setDifficulty(difficulty: AIDifficulty): Promise<void>;
    stop(): void;
    cleanup(): void;
    dispose(): void;
    getAIPlayer(): AIPlayer;
    getPlayer(): AIPlayer;
    isThinking(): boolean;
}

// ===========================================
// ユーティリティ関数
// ===========================================

const getDefaultName = (difficulty: AIDifficulty): string => {
    const names: Record<AIDifficulty, string> = {
        beginner: "AI初心者",
        intermediate: "AI中級者",
        advanced: "AI上級者",
        expert: "AIエキスパート",
    };
    return names[difficulty];
};

// AI Player ID生成関数（各インスタンスで独立したIDを生成）
const generateAIPlayerId = (() => {
    let counter = 0;
    return () => `ai-${Date.now()}-${++counter}`;
})();

const createAIPlayer = (difficulty: AIDifficulty, name?: string): AIPlayer => ({
    id: generateAIPlayerId(),
    name: name || getDefaultName(difficulty),
    difficulty,
    isThinking: false,
});

// ===========================================
// AIサービスファクトリー
// ===========================================

/**
 * AIサービスインスタンスを作成する
 * クロージャーを使用して状態を管理
 */
export const createAIService = (
    difficulty: AIDifficulty = "intermediate",
    name?: string,
): AIService => {
    // インスタンス固有の状態
    let worker: Worker | null = null;
    let requestId = 0;
    const pendingRequests = new Map<
        string,
        {
            resolve: (value: unknown) => void;
            reject: (error: unknown) => void;
        }
    >();
    let initialized = false;
    let aiPlayer = createAIPlayer(difficulty, name);

    // Worker メッセージハンドラー
    const handleWorkerMessage = (event: MessageEvent<AIResponse>): void => {
        const response = event.data;

        if (!response.requestId) return;

        const pending = pendingRequests.get(response.requestId);
        if (!pending) return;

        pendingRequests.delete(response.requestId);

        if (isErrorResponse(response)) {
            pending.reject(new Error(response.error));
        } else {
            pending.resolve(response);
        }
    };

    // リクエスト送信ヘルパー
    const sendRequest = async <T extends AIWorkerMessage>(
        message: Omit<T, "requestId">,
    ): Promise<AIResponse> => {
        if (!worker) {
            throw new Error("Worker not initialized");
        }

        const reqId = `req-${requestId++}`;
        const fullMessage = { ...message, requestId: reqId } as T;

        return new Promise((resolve, reject) => {
            pendingRequests.set(reqId, {
                resolve: resolve as (value: unknown) => void,
                reject,
            });
            worker?.postMessage(fullMessage);

            // Timeout - increased for complex positions
            setTimeout(() => {
                if (pendingRequests.has(reqId)) {
                    pendingRequests.delete(reqId);
                    reject(new Error("Request timeout"));
                }
            }, 60000); // 60 seconds timeout for complex positions
        });
    };

    // 初期化関数
    const initialize = async (): Promise<void> => {
        if (initialized) return;

        try {
            // Create worker
            worker = new Worker(new URL("../../workers/aiWorker.ts", import.meta.url), {
                type: "module",
            });

            // Set up message handler
            worker.addEventListener("message", handleWorkerMessage);

            // Wait for worker ready
            await new Promise<void>((resolve, reject) => {
                const timeout = setTimeout(() => {
                    reject(new Error("Worker initialization timeout"));
                }, 5000);

                const readyHandler = (event: MessageEvent<AIResponse>) => {
                    if (event.data.type === "ready") {
                        clearTimeout(timeout);
                        worker?.removeEventListener("message", readyHandler);
                        resolve();
                    }
                };

                if (worker) {
                    worker.addEventListener("message", readyHandler);
                } else {
                    reject(new Error("Worker not created"));
                }
            });

            // Initialize AI engine
            await sendRequest<InitializeRequest>({
                type: "initialize",
                difficulty: aiPlayer.difficulty,
            });

            initialized = true;
        } catch (error) {
            cleanup();
            throw error;
        }
    };

    // 手を計算
    const calculateMove = async (
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        moveHistory: Move[],
    ): Promise<{ move: Move; evaluation: PositionEvaluation }> => {
        if (!initialized) {
            await initialize();
        }

        // 思考状態を更新（イミュータブルに）
        aiPlayer = { ...aiPlayer, isThinking: true };

        try {
            console.log(
                "[AIService] Sending calculate_move with moveHistory:",
                moveHistory?.length || 0,
            );
            const response = await sendRequest<CalculateMoveRequest>({
                type: "calculate_move",
                board,
                hands,
                currentPlayer,
                moveHistory,
            });

            if (!isMoveCalculatedResponse(response)) {
                throw new Error("Invalid response from worker");
            }

            return { move: response.move, evaluation: response.evaluation };
        } finally {
            aiPlayer = { ...aiPlayer, isThinking: false };
        }
    };

    // 局面評価
    const evaluatePosition = async (
        board: Board,
        hands: Hands,
        currentPlayer: Player,
    ): Promise<PositionEvaluation> => {
        if (!initialized) {
            await initialize();
        }

        const response = await sendRequest<EvaluatePositionRequest>({
            type: "evaluate_position",
            board,
            hands,
            currentPlayer,
        });

        if (!isPositionEvaluatedResponse(response)) {
            throw new Error("Invalid response from worker");
        }

        return response.evaluation;
    };

    // 難易度設定
    const setDifficulty = async (difficulty: AIDifficulty): Promise<void> => {
        if (!initialized) {
            await initialize();
        }

        await sendRequest<SetDifficultyRequest>({
            type: "set_difficulty",
            difficulty,
        });

        aiPlayer = {
            ...aiPlayer,
            difficulty,
            name: getDefaultName(difficulty),
        };
    };

    // 停止
    const stop = (): void => {
        if (worker) {
            worker.postMessage({ type: "stop", requestId: `stop-${requestId++}` });
        }
        aiPlayer = { ...aiPlayer, isThinking: false };
    };

    // クリーンアップ
    const cleanup = (): void => {
        stop();
        if (worker) {
            worker.terminate();
            worker = null;
        }
        initialized = false;
        pendingRequests.clear();
    };

    // 破棄
    const dispose = (): void => {
        cleanup();
    };

    // AIプレイヤー取得
    const getAIPlayer = (): AIPlayer => ({ ...aiPlayer });

    // プレイヤー取得（エイリアス）
    const getPlayer = (): AIPlayer => getAIPlayer();

    // 思考中かどうか
    const isThinking = (): boolean => aiPlayer.isThinking;

    // Public API
    return {
        initialize,
        calculateMove,
        evaluatePosition,
        setDifficulty,
        stop,
        cleanup,
        dispose,
        getAIPlayer,
        getPlayer,
        isThinking,
    };
};
