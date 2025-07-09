import type { GameStatus } from "../../application/service/gameState";
import type { Board } from "../model/board";
import type { Player } from "../model/piece";
import { isCheckmate, isInCheck } from "./checkmate";
import type { Hands } from "./moveService";

/**
 * Creates immutable clones of board and hands for safe state updates
 */
export function cloneBoardAndHands(board: Board, hands: Hands): { board: Board; hands: Hands } {
    // Board は浅い構造なのでスプレッドで OK
    const newBoard: Board = { ...board };
    // Hands はネストしているため plain object 化してから deep copy する
    const newHands: Hands = structuredClone({
        black: { ...hands.black },
        white: { ...hands.white },
    });

    return { board: newBoard, hands: newHands };
}

/**
 * Determines game status based on board state and current player
 */
export function determineGameStatus(board: Board, hands: Hands, currentPlayer: Player): GameStatus {
    if (isInCheck(board, currentPlayer)) {
        if (isCheckmate(board, hands, currentPlayer)) {
            return "checkmate";
        }
        return "check";
    }
    return "playing";
}
