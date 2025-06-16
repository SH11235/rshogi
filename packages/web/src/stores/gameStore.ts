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
        const { board, currentPlayer, selectedSquare, gameStatus } = get();

        // ゲーム終了時は操作不可
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        const squareKey = `${square.row}${square.column}` as keyof Board;
        const piece = board[squareKey];

        // 既に選択されているマスをクリックした場合は選択解除
        if (selectedSquare?.row === square.row && selectedSquare?.column === square.column) {
            set({ selectedSquare: null, validMoves: [] });
            return;
        }

        // 自分の駒を選択
        if (piece && piece.owner === currentPlayer) {
            try {
                const moves = generateMoves(board, square);
                const validMoves = moves.map((m) => m.to);
                set({ selectedSquare: square, validMoves });
                console.log(
                    `Selected piece at ${square.row}${square.column}, valid moves:`,
                    validMoves,
                );
            } catch (error) {
                console.error("Error generating moves:", error);
                set({ selectedSquare: null, validMoves: [] });
            }
        }
        // 移動先を選択
        else if (selectedSquare) {
            const { validMoves } = get();
            const isValidMove = validMoves.some(
                (m) => m.row === square.row && m.column === square.column,
            );
            if (isValidMove) {
                console.log(
                    `Making move from ${selectedSquare.row}${selectedSquare.column} to ${square.row}${square.column}`,
                );
                get().makeMove(selectedSquare, square);
            } else {
                // 無効な移動先を選択した場合、選択を解除
                set({ selectedSquare: null, validMoves: [] });
            }
        }
        // 何も選択していない状態で空のマス或いは相手の駒をクリック
        else {
            set({ selectedSquare: null, validMoves: [] });
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

            // ゲーム状態判定
            let newStatus: GameStatus = "playing";
            if (isInCheck(result.board, nextPlayer)) {
                if (isCheckmate(result.board, result.hands, nextPlayer)) {
                    newStatus = "checkmate";
                } else {
                    newStatus = "check";
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
