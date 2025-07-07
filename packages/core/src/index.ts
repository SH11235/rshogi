// Domain models
export * from "./domain/model/board";
export * from "./domain/model/move";
export * from "./domain/model/piece";
export * from "./domain/model/square";
export * from "./domain/model/history";
export * from "./domain/model/notation";

// Domain services
export * from "./domain/service/moveService";
export * from "./domain/service/checkmate";
export * from "./domain/service/generateDropMoves";
export * from "./domain/service/legalMoves";
export * from "./domain/service/promotionService";
export * from "./domain/service/kifService";
export * from "./domain/service/sfenService";
export * from "./domain/service/moveValidationService";
export * from "./domain/service/timerService";
export * from "./domain/service/repetitionService";
export { checkTryRule } from "./domain/service/repetitionService";
export * from "./domain/service/mateSearch";
export * from "./domain/service/mateSearchBenchmark";
export {
    type GameState,
    type GameStatus,
    type GameMetadata,
    type TimeControl,
    serializeGameState,
    deserializeGameState,
    createInitialGameState,
    updateGameState,
    reconstructGameState,
    reconstructGameStateWithInitial,
} from "./domain/service/gameState";

// Initial board
export * from "./domain/initialBoard";

// Utils
export * from "./utils/i18n";

// Domain utils
export * from "./domain/utils/notation";

// AI types
export * from "./types/ai";

// AI modules
export * from "./ai/engine";
export * from "./ai/evaluation";
export * from "./ai/search";
export * from "./ai/endgameDatabase";
export { OpeningBook } from "./ai/openingBook";
export { generateMainOpenings } from "./ai/openingData";
export type { OpeningEntry, OpeningMove, FindMovesOptions } from "./ai/openingBook";
export type { OpeningBookLoaderInterface } from "./ai/openingBookInterface";
