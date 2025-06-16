// Domain models
export * from "./domain/model/board";
export * from "./domain/model/move";
export * from "./domain/model/piece";
export * from "./domain/model/square";

// Domain services
export * from "./domain/service/moveService";
export * from "./domain/service/checkmate";
export * from "./domain/service/generateDropMoves";
export * from "./domain/service/legalMoves";
export {
    type GameState,
    type GameStatus,
    type GameMetadata,
    type TimeControl,
    serializeGameState,
    deserializeGameState,
    createInitialGameState,
    updateGameState,
} from "./domain/service/gameState";

// Initial board
export * from "./domain/initialBoard";

// Utils
export * from "./utils/i18n";

// Domain utils
export * from "./domain/utils/notation";
