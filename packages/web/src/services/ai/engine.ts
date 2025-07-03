import type { Board, Column, Hands, Move, PieceType, Player, Row } from "shogi-core";
import { generateLegalDropMovesForPiece, generateLegalMoves } from "shogi-core";
import type { AIDifficulty, PositionEvaluation } from "../../types/ai";
import { AI_DIFFICULTY_CONFIGS } from "../../types/ai";
import { evaluatePosition as advancedEvaluatePosition } from "./evaluation";
import { IterativeDeepeningSearch } from "./search";

export class AIEngine {
    private difficulty: AIDifficulty;
    private searchDepth: number;
    private timeLimit: number;
    private startTime = 0;
    private lastEvaluation: PositionEvaluation | null = null;
    private iterativeSearch: IterativeDeepeningSearch;

    constructor(difficulty: AIDifficulty) {
        this.difficulty = difficulty;
        const config = AI_DIFFICULTY_CONFIGS[difficulty];
        this.searchDepth = config.searchDepth || 4;
        this.timeLimit = config.timeLimit || 3000;
        this.iterativeSearch = new IterativeDeepeningSearch();
    }

    setDifficulty(difficulty: AIDifficulty): void {
        this.difficulty = difficulty;
        const config = AI_DIFFICULTY_CONFIGS[difficulty];
        this.searchDepth = config.searchDepth || 4;
        this.timeLimit = config.timeLimit || 3000;
    }

    getSearchDepth(): number {
        return this.searchDepth;
    }

    async calculateBestMove(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        _moveHistory: Move[],
    ): Promise<Move> {
        this.startTime = Date.now();

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

        // Use iterative deepening search for better move ordering and time management
        const searchOptions = {
            maxDepth: this.searchDepth,
            timeLimit: this.timeLimit,
            evaluate: (b: Board, h: Hands, p: Player) => {
                const evaluation = advancedEvaluatePosition(b, h, p);
                return evaluation.total;
            },
            generateMoves: (b: Board, h: Hands, p: Player) => this.generateAllLegalMoves(b, h, p),
        };

        const searchResult = await this.iterativeSearch.search(
            board,
            hands,
            currentPlayer,
            legalMoves,
            searchOptions,
        );

        this.lastEvaluation = {
            score: searchResult.score,
            depth: searchResult.depth,
            pv: searchResult.pv,
            nodes: searchResult.nodes,
            time: searchResult.time,
        };

        return searchResult.bestMove;
    }

    evaluatePosition(board: Board, hands: Hands, player: Player): PositionEvaluation {
        // Use advanced evaluation function
        const evaluation = advancedEvaluatePosition(board, hands, player);

        return {
            score: evaluation.total,
            depth: 0,
            pv: [],
            nodes: 0,
            time: 0,
        };
    }

    generateAllLegalMoves(board: Board, hands: Hands, player: Player): Move[] {
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
        this.iterativeSearch.stop();
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
