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

export class AIPlayerService {
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
            await this.sendRequest<void>({
                type: "initialize",
                difficulty: this.aiPlayer.difficulty,
                requestId: "", // will be replaced in sendRequest
            } as InitializeRequest);

            this.initialized = true;
        } catch (error) {
            console.error("Failed to initialize AI player:", error);
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

    private async sendRequest<T>(message: AIWorkerMessage): Promise<T> {
        if (!this.worker) {
            throw new Error("Worker not initialized");
        }

        const requestId = `req-${++this.requestId}`;
        const fullMessage = { ...message, requestId };

        return new Promise<T>((resolve, reject) => {
            this.pendingRequests.set(requestId, {
                resolve: resolve as (value: unknown) => void,
                reject,
            });
            this.worker?.postMessage(fullMessage);

            // Timeout handling
            setTimeout(() => {
                if (this.pendingRequests.has(requestId)) {
                    this.pendingRequests.delete(requestId);
                    reject(new Error("Request timeout"));
                }
            }, 30000); // 30 second timeout
        });
    }

    async calculateMove(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        moveHistory: Move[],
    ): Promise<Move> {
        if (!this.initialized) {
            await this.initialize();
        }

        this.aiPlayer.isThinking = true;

        try {
            const response = await this.sendRequest<AIResponse>({
                type: "calculate_move",
                board,
                hands,
                currentPlayer,
                moveHistory,
                requestId: "", // will be replaced in sendRequest
            } as CalculateMoveRequest);

            if (!isMoveCalculatedResponse(response)) {
                throw new Error("Invalid response from AI worker");
            }

            this.aiPlayer.lastEvaluation = response.evaluation;
            return response.move;
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

        const response = await this.sendRequest<AIResponse>({
            type: "evaluate_position",
            board,
            hands,
            currentPlayer,
            requestId: "", // will be replaced in sendRequest
        } as EvaluatePositionRequest);

        if (!isPositionEvaluatedResponse(response)) {
            throw new Error("Invalid response from AI worker");
        }

        return response.evaluation;
    }

    async setDifficulty(difficulty: AIDifficulty): Promise<void> {
        if (!this.initialized) {
            this.aiPlayer.difficulty = difficulty;
            return;
        }

        await this.sendRequest<void>({
            type: "set_difficulty",
            difficulty,
            requestId: "", // will be replaced in sendRequest
        } as SetDifficultyRequest);

        this.aiPlayer.difficulty = difficulty;
        this.aiPlayer.name = this.getDefaultName(difficulty);
    }

    stop(): void {
        if (this.worker && this.initialized) {
            this.worker.postMessage({
                type: "stop",
                requestId: `req-${++this.requestId}`,
            });
        }
    }

    getPlayer(): AIPlayer {
        return { ...this.aiPlayer };
    }

    isThinking(): boolean {
        return this.aiPlayer.isThinking;
    }

    getLastEvaluation(): PositionEvaluation | undefined {
        return this.aiPlayer.lastEvaluation;
    }

    dispose(): void {
        this.stop();
        this.pendingRequests.clear();

        if (this.worker) {
            this.worker.terminate();
            this.worker = null;
        }

        this.initialized = false;
    }
}
