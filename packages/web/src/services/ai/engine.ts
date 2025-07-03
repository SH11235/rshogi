import type {
    Board,
    Column,
    Hands,
    Move,
    Piece,
    PieceType,
    Player,
    Row,
    SquareKey,
} from "shogi-core";
import {
    applyMove,
    generateLegalDropMovesForPiece,
    generateLegalMoves,
    isCheckmate,
    isInCheck,
} from "shogi-core";
import type { AIDifficulty, PositionEvaluation } from "../../types/ai";
import { AI_DIFFICULTY_CONFIGS } from "../../types/ai";

// Piece values (in centipawns)
const PIECE_VALUES: Record<PieceType, number> = {
    pawn: 100,
    lance: 430,
    knight: 450,
    silver: 640,
    gold: 690,
    bishop: 890,
    rook: 1040,
    king: 0, // King has no material value
    gyoku: 0,
};

// Promoted piece bonus
const PROMOTION_BONUS: Record<PieceType, number> = {
    pawn: 420, // Tokin is worth much more than pawn
    lance: 260,
    knight: 240,
    silver: 50,
    gold: 0,
    bishop: 150,
    rook: 150,
    king: 0,
    gyoku: 0,
};

// Position tables for piece-square evaluation
const PAWN_TABLE = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 10, 10, 10, 10, 10, 10, 10, 10, 5, 5, 5, 5, 5, 5, 5, 5, 5, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -5, -5, -5, -5, -5,
    -5, -5, -5, -5, -10, -10, -10, -10, -10, -10, -10, -10, -10, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

export class AIEngine {
    private difficulty: AIDifficulty;
    private searchDepth: number;
    private timeLimit: number;
    private startTime = 0;
    private nodesSearched = 0;
    private shouldStop = false;
    private lastEvaluation: PositionEvaluation | null = null;

    constructor(difficulty: AIDifficulty) {
        this.difficulty = difficulty;
        const config = AI_DIFFICULTY_CONFIGS[difficulty];
        this.searchDepth = config.searchDepth || 4;
        this.timeLimit = config.timeLimit || 3000;
    }

    setDifficulty(difficulty: AIDifficulty): void {
        this.difficulty = difficulty;
        const config = AI_DIFFICULTY_CONFIGS[difficulty];
        this.searchDepth = config.searchDepth || 4;
        this.timeLimit = config.timeLimit || 3000;
    }

    async calculateBestMove(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        _moveHistory: Move[],
    ): Promise<Move> {
        this.startTime = Date.now();
        this.nodesSearched = 0;
        this.shouldStop = false;

        // Generate all legal moves
        const legalMoves = this.generateAllLegalMoves(board, hands, currentPlayer);

        if (legalMoves.length === 0) {
            throw new Error("No legal moves available");
        }

        // For beginner level, sometimes make random moves
        if (this.difficulty === "beginner" && Math.random() < 0.3) {
            const randomIndex = Math.floor(Math.random() * legalMoves.length);
            const move = legalMoves[randomIndex];
            this.lastEvaluation = {
                score: 0,
                depth: 1,
                pv: [move],
                nodes: legalMoves.length,
                time: Date.now() - this.startTime,
            };
            return move;
        }

        // Use minimax with alpha-beta pruning
        let bestMove = legalMoves[0];
        let bestScore = Number.NEGATIVE_INFINITY;
        const pv: Move[] = [];

        for (const move of legalMoves) {
            if (this.shouldStop || Date.now() - this.startTime > this.timeLimit) break;

            const newBoard = applyMove(board, hands, currentPlayer, move);
            const score = -this.minimax(
                newBoard.board,
                newBoard.hands,
                currentPlayer === "black" ? "white" : "black",
                this.searchDepth - 1,
                Number.NEGATIVE_INFINITY,
                -bestScore,
            );

            if (score > bestScore) {
                bestScore = score;
                bestMove = move;
                pv[0] = move;
            }
        }

        this.lastEvaluation = {
            score: bestScore,
            depth: this.searchDepth,
            pv,
            nodes: this.nodesSearched,
            time: Date.now() - this.startTime,
        };

        return bestMove;
    }

    private minimax(
        board: Board,
        hands: Hands,
        player: Player,
        depth: number,
        alpha: number,
        beta: number,
    ): number {
        this.nodesSearched++;

        // Time limit check
        if (this.shouldStop || Date.now() - this.startTime > this.timeLimit) {
            return this.evaluatePosition(board, hands, player).score;
        }

        // Terminal node evaluation
        if (depth === 0) {
            return this.evaluatePosition(board, hands, player).score;
        }

        // Check for checkmate
        if (isCheckmate(board, hands, player)) {
            return -100000 + (this.searchDepth - depth); // Prefer faster checkmates
        }

        // Generate moves
        const legalMoves = this.generateAllLegalMoves(board, hands, player);
        if (legalMoves.length === 0) {
            return 0; // Stalemate
        }

        // Search moves
        let maxScore = Number.NEGATIVE_INFINITY;
        let currentAlpha = alpha;
        for (const move of legalMoves) {
            if (currentAlpha >= beta) break; // Alpha-beta cutoff

            const newBoard = applyMove(board, hands, player, move);
            const score = -this.minimax(
                newBoard.board,
                newBoard.hands,
                player === "black" ? "white" : "black",
                depth - 1,
                -beta,
                -currentAlpha,
            );

            maxScore = Math.max(maxScore, score);
            currentAlpha = Math.max(currentAlpha, score);
        }

        return maxScore;
    }

    evaluatePosition(board: Board, hands: Hands, player: Player): PositionEvaluation {
        let score = 0;

        // Material evaluation
        for (const [square, piece] of Object.entries(board)) {
            if (piece) {
                const value = this.getPieceValue(piece);
                const positionBonus = this.getPositionBonus(piece, square as SquareKey);
                const totalValue = value + positionBonus;

                if (piece.owner === player) {
                    score += totalValue;
                } else {
                    score -= totalValue;
                }
            }
        }

        // Hand pieces evaluation
        const handPieces: PieceType[] = [
            "pawn",
            "lance",
            "knight",
            "silver",
            "gold",
            "bishop",
            "rook",
        ];
        for (const pieceType of handPieces) {
            const blackCount = hands.black[pieceType] || 0;
            const whiteCount = hands.white[pieceType] || 0;
            const pieceValue = PIECE_VALUES[pieceType] * 0.9; // Hand pieces slightly less valuable

            if (player === "black") {
                score += blackCount * pieceValue;
                score -= whiteCount * pieceValue;
            } else {
                score += whiteCount * pieceValue;
                score -= blackCount * pieceValue;
            }
        }

        // King safety bonus
        const inCheck = isInCheck(board, player);
        if (inCheck) {
            score -= 50; // Penalty for being in check
        }

        return {
            score,
            depth: 0,
            pv: [],
            nodes: 0,
            time: 0,
        };
    }

    private getPieceValue(piece: Piece): number {
        const baseValue = PIECE_VALUES[piece.type];
        const promotionBonus = piece.promoted ? PROMOTION_BONUS[piece.type] : 0;
        return baseValue + promotionBonus;
    }

    private getPositionBonus(piece: Piece, square: SquareKey): number {
        // Simple position evaluation for pawns
        if (piece.type === "pawn" && !piece.promoted) {
            const row = Number.parseInt(square[0]);
            const col = Number.parseInt(square[1]);
            const index = (row - 1) * 9 + (col - 1);

            // Flip table for white pieces
            const tableIndex = piece.owner === "black" ? index : 80 - index;
            return PAWN_TABLE[tableIndex] || 0;
        }

        // Central control bonus for other pieces
        const row = Number.parseInt(square[0]);
        const col = Number.parseInt(square[1]);
        const centerDistance = Math.abs(row - 5) + Math.abs(col - 5);
        const centralBonus = Math.max(0, 4 - centerDistance) * 5;

        return centralBonus;
    }

    private generateAllLegalMoves(board: Board, hands: Hands, player: Player): Move[] {
        const moves: Move[] = [];

        // Board moves
        for (const [fromSquare, piece] of Object.entries(board)) {
            if (piece && piece.owner === player) {
                const square = {
                    row: Number.parseInt(fromSquare[0]) as Row,
                    column: Number.parseInt(fromSquare[1]) as Column,
                };
                const legalSquares = generateLegalMoves(board, hands, square, player);
                // Convert squares to moves
                for (const toSquare of legalSquares) {
                    const move: Move = {
                        type: "move",
                        from: square,
                        to: toSquare,
                        piece,
                        promote: false,
                        captured: board[`${toSquare.row}${toSquare.column}` as keyof Board] || null,
                    };
                    moves.push(move);

                    // Check if promotion is possible
                    if (
                        (piece.type === "pawn" ||
                            piece.type === "lance" ||
                            piece.type === "knight" ||
                            piece.type === "silver" ||
                            piece.type === "bishop" ||
                            piece.type === "rook") &&
                        !piece.promoted
                    ) {
                        const canPromoteFrom =
                            piece.owner === "black" ? square.row <= 3 : square.row >= 7;
                        const canPromoteTo =
                            piece.owner === "black" ? toSquare.row <= 3 : toSquare.row >= 7;
                        if (canPromoteFrom || canPromoteTo) {
                            const promoteMove: Move = {
                                type: "move",
                                from: move.from,
                                to: move.to,
                                piece: move.piece,
                                promote: true,
                                captured: move.captured,
                            };
                            moves.push(promoteMove);
                        }
                    }
                }
            }
        }

        // Drop moves
        const dropPieces: PieceType[] = [
            "pawn",
            "lance",
            "knight",
            "silver",
            "gold",
            "bishop",
            "rook",
        ];
        for (const pieceType of dropPieces) {
            const count = hands[player][pieceType] || 0;
            if (count > 0) {
                const dropSquares = generateLegalDropMovesForPiece(board, hands, pieceType, player);
                // Convert squares to drop moves
                for (const toSquare of dropSquares) {
                    const dropMove: Move = {
                        type: "drop",
                        to: toSquare,
                        piece: {
                            type: pieceType,
                            owner: player,
                            promoted: false,
                        },
                    };
                    moves.push(dropMove);
                }
            }
        }

        // Shuffle moves for variety (especially for beginner level)
        if (this.difficulty === "beginner" || this.difficulty === "intermediate") {
            for (let i = moves.length - 1; i > 0; i--) {
                const j = Math.floor(Math.random() * (i + 1));
                [moves[i], moves[j]] = [moves[j], moves[i]];
            }
        }

        return moves;
    }

    stop(): void {
        this.shouldStop = true;
    }

    getLastEvaluation(): PositionEvaluation {
        return (
            this.lastEvaluation || {
                score: 0,
                depth: 0,
                pv: [],
                nodes: 0,
                time: 0,
            }
        );
    }
}
