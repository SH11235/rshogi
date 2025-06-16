import {
    type Board,
    type GameStatus,
    type Hands,
    type Move,
    type Piece,
    type PieceType,
    type Player,
    type Square,
    applyMove,
    generateLegalDropMovesForPiece,
    generateLegalMoves,
    initialHands,
    isCheckmate,
    isInCheck,
    modernInitialBoard,
} from "shogi-core";
import { create } from "zustand";

// 成り可能判定のヘルパー関数
function canPromote(piece: Piece, from: Square, to: Square): boolean {
    // 既に成っている駒は成れない
    if (piece.promoted) return false;

    // 王は成れない
    if (piece.type === "king" || piece.type === "gyoku") return false;

    // 金は成れない
    if (piece.type === "gold") return false;

    // 成り可能な条件：敵陣に入る、敵陣から移動する、敵陣内で移動する
    const isBlack = piece.owner === "black";
    const enemyZone = isBlack ? [1, 2, 3] : [7, 8, 9];

    const fromInEnemyZone = enemyZone.includes(from.row);
    const toInEnemyZone = enemyZone.includes(to.row);

    return fromInEnemyZone || toInEnemyZone;
}

// 成りを強制される場合の判定
function mustPromote(piece: Piece, to: Square): boolean {
    if (piece.promoted) return false;

    const isBlack = piece.owner === "black";

    // 歩、香：最奥段で行き場がなくなる
    if (piece.type === "pawn" || piece.type === "lance") {
        return isBlack ? to.row === 1 : to.row === 9;
    }

    // 桂：最奥2段で行き場がなくなる
    if (piece.type === "knight") {
        return isBlack ? to.row <= 2 : to.row >= 8;
    }

    return false;
}

// ドロップ可能位置を計算（王手放置チェック含む）
function getValidDropSquares(
    board: Board,
    hands: Hands,
    pieceType: PieceType,
    player: Player,
): Square[] {
    // 合法手生成機能を使用
    return generateLegalDropMovesForPiece(board, hands, pieceType, player);
}

interface PromotionPendingMove {
    from: Square;
    to: Square;
    piece: Piece;
}

interface GameState {
    board: Board;
    hands: Hands;
    currentPlayer: Player;
    selectedSquare: Square | null;
    selectedDropPiece: { type: PieceType; player: Player } | null;
    validMoves: Square[];
    validDropSquares: Square[];
    moveHistory: Move[];
    gameStatus: GameStatus;
    promotionPending: PromotionPendingMove | null;

    selectSquare: (square: Square) => void;
    selectDropPiece: (pieceType: PieceType, player: Player) => void;
    makeMove: (from: Square, to: Square, promote?: boolean) => void;
    makeDrop: (pieceType: PieceType, to: Square) => void;
    confirmPromotion: (promote: boolean) => void;
    cancelPromotion: () => void;
    resetGame: () => void;
}

export const useGameStore = create<GameState>((set, get) => ({
    board: modernInitialBoard,
    hands: structuredClone(initialHands()),
    currentPlayer: "black",
    selectedSquare: null,
    selectedDropPiece: null,
    validMoves: [],
    validDropSquares: [],
    moveHistory: [],
    gameStatus: "playing",
    promotionPending: null,

    selectSquare: (square: Square) => {
        const {
            board,
            currentPlayer,
            selectedSquare,
            selectedDropPiece,
            validDropSquares,
            gameStatus,
        } = get();

        // ゲーム終了時は操作不可
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        const squareKey = `${square.row}${square.column}` as keyof Board;
        const piece = board[squareKey];

        // ドロップモードの場合
        if (selectedDropPiece) {
            const isValidDrop = validDropSquares.some(
                (s) => s.row === square.row && s.column === square.column,
            );
            if (isValidDrop) {
                get().makeDrop(selectedDropPiece.type, square);
            } else {
                // 無効なドロップ先、選択解除
                set({ selectedDropPiece: null, validDropSquares: [] });
            }
            return;
        }

        // 既に選択されているマスをクリックした場合は選択解除
        if (selectedSquare?.row === square.row && selectedSquare?.column === square.column) {
            set({ selectedSquare: null, validMoves: [] });
            return;
        }

        // 自分の駒を選択
        if (piece && piece.owner === currentPlayer) {
            try {
                const { hands } = get();
                // 王手放置チェックを含む合法手のみを生成
                const validMoves = generateLegalMoves(board, hands, square, currentPlayer);
                set({
                    selectedSquare: square,
                    validMoves,
                    // ドロップモードをクリア
                    selectedDropPiece: null,
                    validDropSquares: [],
                });
                console.log(
                    `Selected piece at ${square.row}${square.column}, legal moves:`,
                    validMoves,
                );
            } catch (error) {
                console.error("Error generating legal moves:", error);
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
                const fromKey = `${selectedSquare.row}${selectedSquare.column}` as keyof Board;
                const movingPiece = board[fromKey];

                if (movingPiece) {
                    // 成りを強制される場合は自動で成る
                    if (mustPromote(movingPiece, square)) {
                        console.log("Forced promotion");
                        get().makeMove(selectedSquare, square, true);
                    }
                    // 成り可能な場合はダイアログを表示
                    else if (canPromote(movingPiece, selectedSquare, square)) {
                        console.log("Showing promotion dialog");
                        set({
                            promotionPending: {
                                from: selectedSquare,
                                to: square,
                                piece: movingPiece,
                            },
                            selectedSquare: null,
                            validMoves: [],
                        });
                    }
                    // 通常の移動
                    else {
                        console.log(
                            `Making move from ${selectedSquare.row}${selectedSquare.column} to ${square.row}${square.column}`,
                        );
                        get().makeMove(selectedSquare, square);
                    }
                }
            } else {
                // 無効な移動先を選択した場合、選択を解除
                set({ selectedSquare: null, validMoves: [] });
            }
        }
        // 何も選択していない状態で空のマス或いは相手の駒をクリック
        else {
            set({
                selectedSquare: null,
                validMoves: [],
                selectedDropPiece: null,
                validDropSquares: [],
            });
        }
    },

    selectDropPiece: (pieceType: PieceType, player: Player) => {
        const { board, hands, currentPlayer, gameStatus } = get();

        // ゲーム終了時は操作不可
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        // 手番プレイヤーでない場合は無視
        if (player !== currentPlayer) {
            return;
        }

        // 同じ駒を再度選択した場合は選択解除
        const { selectedDropPiece } = get();
        if (selectedDropPiece?.type === pieceType && selectedDropPiece?.player === player) {
            set({ selectedDropPiece: null, validDropSquares: [] });
            return;
        }

        const validDropSquares = getValidDropSquares(board, hands, pieceType, player);
        set({
            selectedDropPiece: { type: pieceType, player },
            validDropSquares,
            // 通常の移動モードをクリア
            selectedSquare: null,
            validMoves: [],
        });

        console.log(`Selected drop piece: ${pieceType}, valid drops:`, validDropSquares);
    },

    makeDrop: (pieceType: PieceType, to: Square) => {
        const { board, hands, currentPlayer, moveHistory } = get();

        try {
            const pieceNameMap: Record<PieceType, string> = {
                pawn: "歩",
                lance: "香",
                knight: "桂",
                silver: "銀",
                gold: "金",
                bishop: "角",
                rook: "飛",
                king: "王",
                gyoku: "玉",
            };

            const japaneseName = pieceNameMap[pieceType];
            if (!japaneseName || hands[currentPlayer][japaneseName] <= 0) {
                throw new Error("その駒を持っていません");
            }

            const move: Move = {
                type: "drop",
                to,
                piece: {
                    type: pieceType,
                    owner: currentPlayer,
                    promoted: false,
                },
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
                selectedDropPiece: null,
                validDropSquares: [],
                moveHistory: [...moveHistory, move],
                gameStatus: newStatus,
            });

            console.log(`Dropped ${pieceType} at ${to.row}${to.column}`);
        } catch (error) {
            console.error("Invalid drop:", error);
            // エラー時は選択状態をクリア
            set({ selectedDropPiece: null, validDropSquares: [] });
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

    confirmPromotion: (promote: boolean) => {
        const { promotionPending } = get();
        if (!promotionPending) return;

        const { from, to } = promotionPending;
        get().makeMove(from, to, promote);
        set({ promotionPending: null });
    },

    cancelPromotion: () => {
        set({ promotionPending: null });
    },

    resetGame: () => {
        set({
            board: modernInitialBoard,
            hands: structuredClone(initialHands()),
            currentPlayer: "black",
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            moveHistory: [],
            gameStatus: "playing",
            promotionPending: null,
        });
    },
}));
