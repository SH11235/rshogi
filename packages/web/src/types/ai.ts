import type { Board, Hands, Move, Player } from "shogi-core";

// AI difficulty levels
export type AIDifficulty = "beginner" | "intermediate" | "advanced" | "expert";

// AI configuration
export interface AIConfig {
    difficulty: AIDifficulty;
    searchDepth: number;
    timeLimit: number; // milliseconds
    useOpeningBook: boolean;
    useEndgameDatabase: boolean;
}

// Default configurations for each difficulty
export const AI_DIFFICULTY_CONFIGS: Record<AIDifficulty, Partial<AIConfig>> = {
    beginner: {
        searchDepth: 2,
        timeLimit: 1000,
        useOpeningBook: false,
        useEndgameDatabase: false,
    },
    intermediate: {
        searchDepth: 4,
        timeLimit: 3000,
        useOpeningBook: true,
        useEndgameDatabase: false,
    },
    advanced: {
        searchDepth: 6,
        timeLimit: 5000,
        useOpeningBook: true,
        useEndgameDatabase: true,
    },
    expert: {
        searchDepth: 8,
        timeLimit: 10000,
        useOpeningBook: true,
        useEndgameDatabase: true,
    },
};

// Position evaluation result
export interface PositionEvaluation {
    score: number; // centipawns (100 = 1 pawn advantage)
    depth: number; // search depth achieved
    pv: Move[]; // principal variation (best line)
    nodes: number; // nodes searched
    time: number; // time taken in ms
}

// Worker message types
export interface AIRequest {
    requestId: string;
    type: "initialize" | "calculate_move" | "evaluate_position" | "stop" | "set_difficulty";
}

export interface InitializeRequest extends AIRequest {
    type: "initialize";
    difficulty: AIDifficulty;
}

export interface CalculateMoveRequest extends AIRequest {
    type: "calculate_move";
    board: Board;
    hands: Hands;
    currentPlayer: Player;
    moveHistory: Move[];
}

export interface EvaluatePositionRequest extends AIRequest {
    type: "evaluate_position";
    board: Board;
    hands: Hands;
    currentPlayer: Player;
}

export interface StopRequest extends AIRequest {
    type: "stop";
}

export interface SetDifficultyRequest extends AIRequest {
    type: "set_difficulty";
    difficulty: AIDifficulty;
}

export type AIWorkerMessage =
    | InitializeRequest
    | CalculateMoveRequest
    | EvaluatePositionRequest
    | StopRequest
    | SetDifficultyRequest;

// Worker response types
export interface AIResponse {
    type:
        | "ready"
        | "initialized"
        | "move_calculated"
        | "position_evaluated"
        | "stopped"
        | "difficulty_set"
        | "error";
    requestId?: string;
}

export interface MoveCalculatedResponse extends AIResponse {
    type: "move_calculated";
    move: Move;
    evaluation: PositionEvaluation;
}

export interface PositionEvaluatedResponse extends AIResponse {
    type: "position_evaluated";
    evaluation: PositionEvaluation;
}

export interface ErrorResponse extends AIResponse {
    type: "error";
    error: string;
}

// AI player interface for game integration
export interface AIPlayer {
    id: string;
    name: string;
    difficulty: AIDifficulty;
    isThinking: boolean;
    lastEvaluation?: PositionEvaluation;
}

// Type guards
export function isMoveCalculatedResponse(response: AIResponse): response is MoveCalculatedResponse {
    return response.type === "move_calculated";
}

export function isPositionEvaluatedResponse(
    response: AIResponse,
): response is PositionEvaluatedResponse {
    return response.type === "position_evaluated";
}

export function isErrorResponse(response: AIResponse): response is ErrorResponse {
    return response.type === "error";
}

// Search related types
export interface SearchOptions {
    maxDepth: number;
    timeLimit: number;
    evaluate: (board: Board, hands: Hands, player: Player) => number;
    generateMoves: (board: Board, hands: Hands, player: Player) => Move[];
}

export interface SearchResult {
    bestMove: Move;
    score: number;
    depth: number;
    pv: Move[];
    nodes: number;
    time: number;
}

export interface TranspositionEntry {
    score: number;
    depth: number;
    type: "exact" | "lowerbound" | "upperbound";
    bestMove?: Move;
}
