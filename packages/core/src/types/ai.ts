import type { Board } from "../domain/model/board";
import type { Move } from "../domain/model/move";
import type { Player } from "../domain/model/piece";
import type { Hands } from "../domain/service/moveService";

// AI difficulty levels
export type AIDifficulty = "beginner" | "intermediate" | "advanced" | "expert";

// AI configuration
export interface AIConfig {
    difficulty: AIDifficulty;
    searchDepth: number;
    timeLimit: number; // milliseconds
    useEndgameDatabase: boolean;
}

// Default configurations for each difficulty
export const AI_DIFFICULTY_CONFIGS: Record<AIDifficulty, Partial<AIConfig>> = {
    beginner: {
        searchDepth: 2,
        timeLimit: 1000,
        useEndgameDatabase: false,
    },
    intermediate: {
        searchDepth: 4,
        timeLimit: 3000,
        useEndgameDatabase: false,
    },
    advanced: {
        searchDepth: 6,
        timeLimit: 5000,
        useEndgameDatabase: true,
    },
    expert: {
        searchDepth: 8,
        timeLimit: 30000, // Increased to 30 seconds for complex positions
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

// AI player interface for game integration
export interface AIPlayer {
    id: string;
    name: string;
    difficulty: AIDifficulty;
    isThinking: boolean;
    lastEvaluation?: PositionEvaluation;
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
