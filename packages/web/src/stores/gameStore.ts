import {
    type Board,
    type GameStatus,
    type Hands,
    type Move,
    type Player,
    type Square,
    applyMove,
    generateMoves,
    initialHands,
    isCheckmate,
    isInCheck,
    modernInitialBoard,
} from "shogi-core";
import { create } from "zustand";

interface GameState {
    board: Board;
    hands: Hands;
    currentPlayer: Player;
    selectedSquare: Square | null;
    validMoves: Square[];
    moveHistory: Move[];
    gameStatus: GameStatus;

    selectSquare: (square: Square) => void;
    makeMove: (from: Square, to: Square, promote?: boolean) => void;
    resetGame: () => void;
}

export const useGameStore = create<GameState>((set, get) => ({
    board: modernInitialBoard,
    hands: structuredClone(initialHands()),
    currentPlayer: "black",
    selectedSquare: null,
    validMoves: [],
    moveHistory: [],
    gameStatus: "playing",

    selectSquare: (square: Square) => {
        const { board, currentPlayer, selectedSquare } = get();
        const squareKey = `${square.row}${square.column}` as keyof Board;
        const piece = board[squareKey];

        // 既に選択されているマスをクリックした場合は選択解除
        if (selectedSquare?.row === square.row && selectedSquare?.column === square.column) {
            set({ selectedSquare: null, validMoves: [] });
            return;
        }

        // 自分の駒を選択
        if (piece && piece.owner === currentPlayer) {
            const moves = generateMoves(board, square);
            const validMoves = moves.map((m) => m.to);
            set({ selectedSquare: square, validMoves });
        }
        // 移動先を選択
        else if (selectedSquare) {
            const { validMoves } = get();
            const isValidMove = validMoves.some(
                (m) => m.row === square.row && m.column === square.column,
            );
            if (isValidMove) {
                get().makeMove(selectedSquare, square);
            }
        }
    },

    makeMove: (from: Square, to: Square, promote = false) => {
        const { board, hands, currentPlayer, moveHistory } = get();

        try {
            const fromKey = `${from.row}${from.column}` as keyof Board;
            const toKey = `${to.row}${to.column}` as keyof Board;
            const piece = board[fromKey];
            const captured = board[toKey];

            if (!piece) return;

            const move: Move = {
                type: "move",
                from,
                to,
                piece,
                promote,
                captured: captured || null,
            };

            const result = applyMove(board, hands, currentPlayer, move);
            const nextPlayer = currentPlayer === "black" ? "white" : "black";

            // チェックメイト判定
            let newStatus: GameStatus = "playing";
            if (isInCheck(result.board, nextPlayer)) {
                if (isCheckmate(result.board, result.hands, nextPlayer)) {
                    newStatus = currentPlayer === "black" ? "black_win" : "white_win";
                }
            }

            set({
                board: result.board,
                hands: result.hands,
                currentPlayer: nextPlayer,
                selectedSquare: null,
                validMoves: [],
                moveHistory: [...moveHistory, move],
                gameStatus: newStatus,
            });
        } catch (error) {
            console.error("Invalid move:", error);
        }
    },

    resetGame: () => {
        set({
            board: modernInitialBoard,
            hands: structuredClone(initialHands()),
            currentPlayer: "black",
            selectedSquare: null,
            validMoves: [],
            moveHistory: [],
            gameStatus: "playing",
        });
    },
}));
