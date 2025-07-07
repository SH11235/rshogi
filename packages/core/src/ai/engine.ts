import type { Board } from "../domain/model/board";
import type { Move } from "../domain/model/move";
import type { Piece, Player } from "../domain/model/piece";
import type { Column, Row } from "../domain/model/square";
import { generateAllDropMoves } from "../domain/service/generateDropMoves";
import { generateLegalMoves } from "../domain/service/legalMoves";
import type { Hands } from "../domain/service/moveService";
import { mustPromote } from "../domain/service/promotionService";
import type { PositionState } from "../domain/service/repetitionService";
import type { AIDifficulty, PositionEvaluation } from "../types/ai";
import { AI_DIFFICULTY_CONFIGS } from "../types/ai";
import { evaluatePosition as advancedEvaluatePosition } from "./evaluation";
import type { OpeningBook } from "./openingBook";
import type { OpeningBookLoaderInterface } from "./openingBookInterface";
import { IterativeDeepeningSearch } from "./search";

export interface AIEngineConfig {
    difficulty: AIDifficulty;
    searchDepth: number;
    timeLimit: number;
    useOpeningBook: boolean;
}

export class AIEngine {
    private difficulty: AIDifficulty;
    private searchDepth: number;
    private timeLimit: number;
    private startTime = 0;
    private lastEvaluation: PositionEvaluation | null = null;
    private iterativeSearch: IterativeDeepeningSearch;
    private openingBook: OpeningBook | null = null;
    private openingBookLoader: OpeningBookLoaderInterface;
    private useOpeningBook: boolean;

    constructor(difficulty: AIDifficulty, openingBookLoader: OpeningBookLoaderInterface) {
        this.difficulty = difficulty;
        const config = AI_DIFFICULTY_CONFIGS[difficulty];
        this.searchDepth = config.searchDepth || 4;
        this.timeLimit = config.timeLimit || 3000;
        this.iterativeSearch = new IterativeDeepeningSearch();

        // 定跡の初期化
        this.openingBookLoader = openingBookLoader;
        // 初心者レベルでは定跡を使用しない
        this.useOpeningBook = difficulty !== "beginner";
    }

    setDifficulty(difficulty: AIDifficulty): void {
        this.difficulty = difficulty;
        const config = AI_DIFFICULTY_CONFIGS[difficulty];
        this.searchDepth = config.searchDepth || 4;
        this.timeLimit = config.timeLimit || 3000;
        // 初心者レベルでは定跡を使用しない
        this.useOpeningBook = difficulty !== "beginner";
    }

    /**
     * 定跡を読み込む
     */
    async loadOpeningBook(): Promise<void> {
        if (!this.useOpeningBook) {
            this.openingBook = null;
            return;
        }

        try {
            this.openingBook = await this.openingBookLoader.loadForDifficulty(this.difficulty);
        } catch (error) {
            console.warn("Failed to load opening book:", error);
            // フォールバックを使用
            this.openingBook = this.openingBookLoader.loadFromFallback();
        }
    }

    /**
     * 設定を取得
     */
    getConfig(): AIEngineConfig {
        return {
            difficulty: this.difficulty,
            searchDepth: this.searchDepth,
            timeLimit: this.timeLimit,
            useOpeningBook: this.useOpeningBook,
        };
    }

    /**
     * 設定を更新
     */
    setConfig(config: Partial<AIEngineConfig>): void {
        if (config.useOpeningBook !== undefined) {
            this.useOpeningBook = config.useOpeningBook;
        }
        if (config.searchDepth !== undefined) {
            this.searchDepth = config.searchDepth;
        }
        if (config.timeLimit !== undefined) {
            this.timeLimit = config.timeLimit;
        }
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
        console.log(
            "[AIEngine] calculateBestMove called with moveHistory:",
            moveHistory?.length || 0,
        );

        // 定跡をチェック
        if (this.useOpeningBook && this.openingBook) {
            const positionState: PositionState = {
                board,
                hands,
                currentPlayer,
            };

            const openingMoves = this.openingBook.findMoves(positionState, {
                randomize: true,
                moveHistory,
            });
            if (openingMoves.length > 0) {
                // 重み付きランダムで選択される（findMovesでrandomize: true）
                const selectedMove = openingMoves[0];
                this.lastEvaluation = {
                    score: 0,
                    depth: selectedMove.depth || 0,
                    pv: [selectedMove.move],
                    nodes: 1,
                    time: Date.now() - this.startTime,
                };
                return selectedMove.move;
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

        // Return the last evaluation if available to provide search information
        if (this.lastEvaluation) {
            return {
                score: evaluation.total,
                depth: this.lastEvaluation.depth,
                pv: this.lastEvaluation.pv,
                nodes: this.lastEvaluation.nodes,
                time: this.lastEvaluation.time,
            };
        }

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
                            // Check if promotion is forced
                            if (mustPromote(piece as Piece, toSquare)) {
                                // Replace the non-promoted move with promoted version
                                const lastMove = moves[moves.length - 1];
                                if (lastMove.type === "move") {
                                    lastMove.promote = true;
                                }
                            } else {
                                // Add promoted version as an option
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
        }

        // Drop moves - generate once and push all
        const dropMoves = generateAllDropMoves(board, hands, player);
        moves.push(...dropMoves);

        // Shuffle moves for variety (only for beginner level)
        if (this.difficulty === "beginner") {
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
