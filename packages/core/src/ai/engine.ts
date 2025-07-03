import type { Board } from "../domain/model/board";
import type { Move } from "../domain/model/move";
import type { Piece, PieceType, Player } from "../domain/model/piece";
import type { Column, Row } from "../domain/model/square";
import { generateAllDropMoves } from "../domain/service/generateDropMoves";
import { generateLegalMoves } from "../domain/service/legalMoves";
import type { Hands } from "../domain/service/moveService";
import type { AIDifficulty, PositionEvaluation } from "../types/ai";
import { AI_DIFFICULTY_CONFIGS } from "../types/ai";
import { EndgameDatabase } from "./endgameDatabase";
import { evaluatePosition as advancedEvaluatePosition } from "./evaluation";
import { OpeningBook, type OpeningEntry } from "./openingBook";
import { IterativeDeepeningSearch } from "./search";

export class AIEngine {
    private difficulty: AIDifficulty;
    private searchDepth: number;
    private timeLimit: number;
    private startTime = 0;
    private lastEvaluation: PositionEvaluation | null = null;
    private iterativeSearch: IterativeDeepeningSearch;
    private openingBook: OpeningBook;
    private endgameDatabase: EndgameDatabase;
    private useOpeningBook: boolean;

    constructor(difficulty: AIDifficulty) {
        this.difficulty = difficulty;
        const config = AI_DIFFICULTY_CONFIGS[difficulty];
        this.searchDepth = config.searchDepth || 4;
        this.timeLimit = config.timeLimit || 3000;
        this.useOpeningBook = config.useOpeningBook || false;
        this.iterativeSearch = new IterativeDeepeningSearch();

        // 定跡データベースを初期化
        this.openingBook = new OpeningBook();

        // 終盤データベースを初期化
        this.endgameDatabase = new EndgameDatabase();
    }

    setDifficulty(difficulty: AIDifficulty): void {
        this.difficulty = difficulty;
        const config = AI_DIFFICULTY_CONFIGS[difficulty];
        this.searchDepth = config.searchDepth || 4;
        this.timeLimit = config.timeLimit || 3000;
        this.useOpeningBook = config.useOpeningBook || false;
    }

    getSearchDepth(): number {
        return this.searchDepth;
    }

    async calculateBestMove(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        moveHistory: Move[],
    ): Promise<Move> {
        this.startTime = Date.now();

        // 終盤データベースをチェック
        const endgameEval = this.endgameDatabase.evaluate(board, hands, currentPlayer);
        if (endgameEval) {
            // 詰みが見つかった場合
            if (endgameEval.type === "mate" && endgameEval.movesToMate === 1) {
                const mateMove = this.endgameDatabase.searchMate(board, hands, currentPlayer, 1);
                if (mateMove) {
                    this.lastEvaluation = {
                        score: endgameEval.score,
                        depth: 1,
                        pv: [mateMove],
                        nodes: 1,
                        time: Date.now() - this.startTime,
                    };
                    return mateMove;
                }
            }
        }

        // 定跡データベースをチェック（序盤のみ）
        if (this.useOpeningBook && moveHistory.length < 20) {
            const bookMove = this.openingBook.getMove(board, hands, moveHistory.length);
            if (bookMove) {
                // 定跡の手が見つかった場合
                this.lastEvaluation = {
                    score: 0, // 定跡は互角と評価
                    depth: 0,
                    pv: [bookMove],
                    nodes: 1,
                    time: Date.now() - this.startTime,
                };
                return bookMove;
            }
        }

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
                // 終盤データベースをチェック
                const endgameEval = this.endgameDatabase.evaluate(b, h, p);
                if (endgameEval) {
                    return endgameEval.score;
                }

                // 通常の評価関数
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
            if (piece && (piece as Piece).owner === player) {
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
                        ((piece as Piece).type === "pawn" ||
                            (piece as Piece).type === "lance" ||
                            (piece as Piece).type === "knight" ||
                            (piece as Piece).type === "silver" ||
                            (piece as Piece).type === "bishop" ||
                            (piece as Piece).type === "rook") &&
                        !(piece as Piece).promoted
                    ) {
                        const canPromoteFrom =
                            (piece as Piece).owner === "black" ? square.row <= 3 : square.row >= 7;
                        const canPromoteTo =
                            (piece as Piece).owner === "black"
                                ? toSquare.row <= 3
                                : toSquare.row >= 7;
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
                // generateAllDropMovesで特定の駒のdrop movesのみをフィルタリング
                const allDropMoves = generateAllDropMoves(board, hands, player);
                const pieceDrps = allDropMoves.filter((m) => m.piece.type === pieceType);
                moves.push(...pieceDrps);
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

    // 定跡データをロードするメソッドを追加
    loadOpeningBook(data: OpeningEntry[]): void {
        if (this.useOpeningBook) {
            this.openingBook.loadFromData(data);
        }
    }
}
