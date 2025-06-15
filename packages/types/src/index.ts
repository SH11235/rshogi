// Re-export core types for convenience
export type {
    Player,
    Piece,
    PieceType,
    HandPieceType,
    Board,
    Move,
    NormalMove,
    DropMove,
    Square,
    Row,
    Column,
    SquareKey,
    GameState,
    GameStatus,
    GameMetadata,
    TimeControl,
    Hands,
} from "shogi-core";

// API types for client-server communication
export interface GameSessionRequest {
    gameId?: string;
    playerId: string;
    playerName?: string;
}

export interface GameSessionResponse {
    gameId: string;
    playerId: string;
    playerColor: import("shogi-core").Player;
    gameState: import("shogi-core").GameState;
}

export interface MoveRequest {
    gameId: string;
    playerId: string;
    move: import("shogi-core").Move;
}

export interface MoveResponse {
    success: boolean;
    gameState?: import("shogi-core").GameState;
    error?: string;
}

// WebSocket message types
export type WebSocketMessage =
    | { type: "gameStateUpdate"; gameState: import("shogi-core").GameState }
    | { type: "playerJoined"; playerId: string; playerName?: string }
    | { type: "playerLeft"; playerId: string }
    | { type: "gameEnded"; result: { winner?: import("shogi-core").Player; reason: string } }
    | { type: "error"; message: string };

// Discord bot types
export interface DiscordGameSession {
    channelId: string;
    blackPlayer: string; // Discord user ID
    whitePlayer: string; // Discord user ID
    gameState: import("shogi-core").GameState;
}

export interface DiscordMoveCommand {
    userId: string;
    move: string; // Text notation like "7六歩"
}
