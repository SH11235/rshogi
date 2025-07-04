import type { AIDifficulty, Board, Hands, Move, Player, PositionEvaluation } from "shogi-core";

// Re-export core AI types for backward compatibility
export type {
    AIDifficulty,
    PositionEvaluation,
    AIPlayer,
    AIConfig,
    SearchOptions,
    SearchResult,
    TranspositionEntry,
} from "shogi-core";

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
        | "error"
        | "opening_book_progress";
    requestId?: string;
    progress?: number;
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
