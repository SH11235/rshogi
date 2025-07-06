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

export class AIService {
    private worker: Worker | null = null;
    private requestId = 0;
    private pendingRequests = new Map<
        string,
        {
            resolve: (value: unknown) => void;
            reject: (error: unknown) => void;
        }
    >();
    private initialized = false;
    private aiPlayer: AIPlayer;

    constructor(difficulty: AIDifficulty = "intermediate", name?: string) {
        this.aiPlayer = {
            id: `ai-${Date.now()}`,
            name: name || this.getDefaultName(difficulty),
            difficulty,
            isThinking: false,
        };
    }

    private getDefaultName(difficulty: AIDifficulty): string {
        const names: Record<AIDifficulty, string> = {
            beginner: "AI初心者",
            intermediate: "AI中級者",
            advanced: "AI上級者",
            expert: "AIエキスパート",
        };
        return names[difficulty];
    }

    async initialize(): Promise<void> {
        if (this.initialized) return;

        try {
            // Create worker
            this.worker = new Worker(new URL("../../workers/aiWorker.ts", import.meta.url), {
                type: "module",
            });

            // Set up message handler
            this.worker.addEventListener("message", this.handleWorkerMessage.bind(this));

            // Wait for worker ready
            await new Promise<void>((resolve, reject) => {
                const timeout = setTimeout(() => {
                    reject(new Error("Worker initialization timeout"));
                }, 5000);

                const readyHandler = (event: MessageEvent<AIResponse>) => {
                    if (event.data.type === "ready") {
                        clearTimeout(timeout);
                        this.worker?.removeEventListener("message", readyHandler);
                        resolve();
                    }
                };

                this.worker?.addEventListener("message", readyHandler);
            });

            // Initialize AI engine
            await this.sendRequest<InitializeRequest>({
                type: "initialize",
                difficulty: this.aiPlayer.difficulty,
            });

            this.initialized = true;
        } catch (error) {
            this.cleanup();
            throw error;
        }
    }

    private handleWorkerMessage(event: MessageEvent<AIResponse>): void {
        const response = event.data;

        if (!response.requestId) return;

        const pending = this.pendingRequests.get(response.requestId);
        if (!pending) return;

        this.pendingRequests.delete(response.requestId);

        if (isErrorResponse(response)) {
            pending.reject(new Error(response.error));
        } else {
            pending.resolve(response);
        }
    }

    private async sendRequest<T extends AIWorkerMessage>(
        message: Omit<T, "requestId">,
    ): Promise<AIResponse> {
        if (!this.worker) {
            throw new Error("Worker not initialized");
        }

        const requestId = `req-${this.requestId++}`;
        const fullMessage = { ...message, requestId } as T;

        return new Promise((resolve, reject) => {
            this.pendingRequests.set(requestId, {
                resolve: resolve as (value: unknown) => void,
                reject,
            });
            this.worker?.postMessage(fullMessage);

            // Timeout - increased for complex positions
            setTimeout(() => {
                if (this.pendingRequests.has(requestId)) {
                    this.pendingRequests.delete(requestId);
                    reject(new Error("Request timeout"));
                }
            }, 60000); // 60 seconds timeout for complex positions
        });
    }

    async calculateMove(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        moveHistory: Move[],
    ): Promise<{ move: Move; evaluation: PositionEvaluation }> {
        if (!this.initialized) {
            await this.initialize();
        }

        this.aiPlayer.isThinking = true;

        try {
            console.log(
                "[AIService] Sending calculate_move with moveHistory:",
                moveHistory?.length || 0,
            );
            const response = await this.sendRequest<CalculateMoveRequest>({
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
            this.aiPlayer.isThinking = false;
        }
    }

    async evaluatePosition(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
    ): Promise<PositionEvaluation> {
        if (!this.initialized) {
            await this.initialize();
        }

        const response = await this.sendRequest<EvaluatePositionRequest>({
            type: "evaluate_position",
            board,
            hands,
            currentPlayer,
        });

        if (!isPositionEvaluatedResponse(response)) {
            throw new Error("Invalid response from worker");
        }

        return response.evaluation;
    }

    async setDifficulty(difficulty: AIDifficulty): Promise<void> {
        if (!this.initialized) {
            await this.initialize();
        }

        await this.sendRequest<SetDifficultyRequest>({
            type: "set_difficulty",
            difficulty,
        });

        this.aiPlayer.difficulty = difficulty;
        this.aiPlayer.name = this.getDefaultName(difficulty);
    }

    stop(): void {
        if (this.worker) {
            this.worker.postMessage({ type: "stop", requestId: `stop-${this.requestId++}` });
        }
        this.aiPlayer.isThinking = false;
    }

    cleanup(): void {
        this.stop();
        if (this.worker) {
            this.worker.terminate();
            this.worker = null;
        }
        this.initialized = false;
        this.pendingRequests.clear();
    }

    dispose(): void {
        this.cleanup();
    }

    getAIPlayer(): AIPlayer {
        return { ...this.aiPlayer };
    }

    getPlayer(): AIPlayer {
        return this.getAIPlayer();
    }

    isThinking(): boolean {
        return this.aiPlayer.isThinking;
    }
}
