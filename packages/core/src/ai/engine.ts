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
import type { OpeningBookInterface } from "./openingBookInterface";
import type { OpeningBookLoaderInterface } from "./openingBookInterface";
import { IterativeDeepeningSearch } from "./search";

export interface AIEngineConfig {
    difficulty: AIDifficulty;
    searchDepth: number;
    timeLimit: number;
    useOpeningBook: boolean;
}

export interface AIEngineInterface {
    setDifficulty(difficulty: AIDifficulty): void;
    loadOpeningBook(): Promise<void>;
    getConfig(): AIEngineConfig;
    setConfig(config: Partial<AIEngineConfig>): void;
    getSearchDepth(): number;
    calculateBestMove(
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        moveHistory: Move[],
    ): Promise<Move>;
    evaluatePosition(board: Board, hands: Hands, player: Player): PositionEvaluation;
    generateAllLegalMoves(board: Board, hands: Hands, player: Player): Move[];
    stop(): void;
    getLastEvaluation(): PositionEvaluation;
}

// ヘルパー関数：座標からキーを作成
export const squareToKey = (row: Row, column: Column): keyof Board => {
    return `${row}${column}` as keyof Board;
};

// ヘルパー関数：キーから座標を作成
export const keyToSquare = (key: string): { row: Row; column: Column } => {
    return {
        row: Number.parseInt(key[0]) as Row,
        column: Number.parseInt(key[1]) as Column,
    };
};

// 成れる駒の種類かチェック
export const canPromotePieceType = (piece: Piece): boolean => {
    return (
        (piece.type === "pawn" ||
            piece.type === "lance" ||
            piece.type === "knight" ||
            piece.type === "silver" ||
            piece.type === "bishop" ||
            piece.type === "rook") &&
        !piece.promoted
    );
};

// 成れる位置かチェック
export const isPromotablePosition = (
    from: { row: Row },
    to: { row: Row },
    owner: Player,
): boolean => {
    const canPromoteFrom = owner === "black" ? from.row <= 3 : from.row >= 7;
    const canPromoteTo = owner === "black" ? to.row <= 3 : to.row >= 7;
    return canPromoteFrom || canPromoteTo;
};

// 盤上の駒の移動を生成
const generateBoardMoves = (board: Board, player: Player): Move[] => {
    const moves: Move[] = [];

    for (const [squareKey, piece] of Object.entries(board)) {
        if (piece && piece.owner === player) {
            const from = keyToSquare(squareKey);
            const legalSquares = generateLegalMoves(board, {} as Hands, from, player);

            for (const to of legalSquares) {
                const capturedPiece = board[squareToKey(to.row, to.column)] || null;
                const move: Move = {
                    type: "move",
                    from,
                    to,
                    piece,
                    promote: false,
                    captured: capturedPiece,
                };
                moves.push(move);

                // 成り移動の生成
                if (canPromotePieceType(piece) && isPromotablePosition(from, to, player)) {
                    if (mustPromote(piece, to)) {
                        // 必須成りの場合は元の移動を成り移動に置き換え
                        const lastMove = moves[moves.length - 1];
                        if (lastMove.type === "move") {
                            lastMove.promote = true;
                        }
                    } else {
                        // 任意成りの場合は成り移動を追加
                        moves.push({
                            ...move,
                            promote: true,
                        });
                    }
                }
            }
        }
    }

    return moves;
};

// 初心者向けに手をシャッフル
const shuffleMovesForBeginner = (moves: Move[]): Move[] => {
    const shuffled = [...moves];
    for (let i = shuffled.length - 1; i > 0; i--) {
        const j = Math.floor(Math.random() * (i + 1));
        [shuffled[i], shuffled[j]] = [shuffled[j], shuffled[i]];
    }
    return shuffled;
};

// 全ての合法手を生成（分割された関数を使用）
const generateAllLegalMovesInternal = (
    board: Board,
    hands: Hands,
    player: Player,
    difficulty: AIDifficulty,
): Move[] => {
    const boardMoves = generateBoardMoves(board, player);
    const dropMoves = generateAllDropMoves(board, hands, player);
    const allMoves = [...boardMoves, ...dropMoves];

    // 初心者レベルの場合はシャッフル
    return difficulty === "beginner" ? shuffleMovesForBeginner(allMoves) : allMoves;
};

// 内部設定の型
export type InternalAIConfig = {
    readonly difficulty: AIDifficulty;
    readonly searchDepth: number;
    readonly timeLimit: number;
    readonly useOpeningBook: boolean;
};

// 設定の更新関数
export const updateAIConfig = (
    current: InternalAIConfig,
    updates: Partial<AIEngineConfig>,
): InternalAIConfig => {
    const newDifficulty = updates.difficulty ?? current.difficulty;
    const difficultyConfig = updates.difficulty ? AI_DIFFICULTY_CONFIGS[newDifficulty] : null;

    return {
        difficulty: newDifficulty,
        searchDepth: updates.searchDepth ?? difficultyConfig?.searchDepth ?? current.searchDepth,
        timeLimit: updates.timeLimit ?? difficultyConfig?.timeLimit ?? current.timeLimit,
        useOpeningBook:
            updates.useOpeningBook ?? (newDifficulty !== "beginner" && current.useOpeningBook),
    };
};

// 難易度から初期設定を作成
export const createInitialConfig = (difficulty: AIDifficulty): InternalAIConfig => {
    const config = AI_DIFFICULTY_CONFIGS[difficulty];
    return {
        difficulty,
        searchDepth: config.searchDepth || 4,
        timeLimit: config.timeLimit || 3000,
        useOpeningBook: difficulty !== "beginner",
    };
};

/**
 * AIエンジンのファクトリー関数
 */
export function createAIEngine(
    difficulty: AIDifficulty,
    openingBookLoader: OpeningBookLoaderInterface,
): AIEngineInterface {
    // 内部状態
    let config = createInitialConfig(difficulty);
    let startTime = 0;
    let lastEvaluation: PositionEvaluation | null = null;
    const iterativeSearch = new IterativeDeepeningSearch();
    let openingBook: OpeningBookInterface | null = null;

    // 難易度設定
    const setDifficulty = (newDifficulty: AIDifficulty): void => {
        config = updateAIConfig(config, { difficulty: newDifficulty });
    };

    // 定跡の読み込み
    const loadOpeningBook = async (): Promise<void> => {
        if (!config.useOpeningBook) {
            openingBook = null;
            return;
        }

        try {
            openingBook = await openingBookLoader.loadForDifficulty(config.difficulty);
        } catch (error) {
            console.error("Failed to load opening book:", error);
            openingBook = null;
        }
    };

    // 設定の取得
    const getConfig = (): AIEngineConfig => {
        return {
            difficulty: config.difficulty,
            searchDepth: config.searchDepth,
            timeLimit: config.timeLimit,
            useOpeningBook: config.useOpeningBook,
        };
    };

    // 設定の更新
    const setConfig = (updates: Partial<AIEngineConfig>): void => {
        config = updateAIConfig(config, updates);
    };

    // 探索深度の取得
    const getSearchDepth = (): number => {
        return config.searchDepth;
    };

    // 全合法手生成（公開API）
    const generateAllLegalMoves = (board: Board, hands: Hands, player: Player): Move[] => {
        return generateAllLegalMovesInternal(board, hands, player, config.difficulty);
    };

    // 最良手の計算
    const calculateBestMove = async (
        board: Board,
        hands: Hands,
        currentPlayer: Player,
        moveHistory: Move[],
    ): Promise<Move> => {
        startTime = Date.now();
        console.log(
            "[AIEngine] calculateBestMove called with moveHistory:",
            moveHistory?.length || 0,
        );

        // 定跡をチェック
        if (config.useOpeningBook && openingBook) {
            const positionState: PositionState = {
                board,
                hands,
                currentPlayer,
            };

            const openingMoves = openingBook.findMoves(positionState, {
                randomize: true,
                moveHistory,
            });
            if (openingMoves.length > 0) {
                // 重み付きランダムで選択される（findMovesでrandomize: true）
                const selectedMove = openingMoves[0];
                lastEvaluation = {
                    score: 0,
                    depth: selectedMove.depth || 0,
                    pv: [selectedMove.move],
                    nodes: 1,
                    time: Date.now() - startTime,
                };
                return selectedMove.move;
            }
        }

        // Generate all legal moves
        const legalMoves = generateAllLegalMoves(board, hands, currentPlayer);

        if (legalMoves.length === 0) {
            throw new Error("No legal moves available");
        }

        // For beginner level, sometimes make random moves
        if (config.difficulty === "beginner" && Math.random() < 0.3) {
            const randomIndex = Math.floor(Math.random() * legalMoves.length);
            const move = legalMoves[randomIndex];
            lastEvaluation = {
                score: 0,
                depth: 1,
                pv: [move],
                nodes: legalMoves.length,
                time: Date.now() - startTime,
            };
            return move;
        }

        // Use iterative deepening search for better move ordering and time management
        const searchOptions = {
            maxDepth: config.searchDepth,
            timeLimit: config.timeLimit,
            evaluate: (b: Board, h: Hands, p: Player) => {
                // 通常の評価関数
                const evaluation = advancedEvaluatePosition(b, h, p);
                return evaluation.total;
            },
            generateMoves: (b: Board, h: Hands, p: Player) => generateAllLegalMoves(b, h, p),
        };

        const searchResult = await iterativeSearch.search(
            board,
            hands,
            currentPlayer,
            legalMoves,
            searchOptions,
        );

        lastEvaluation = {
            score: searchResult.score,
            depth: searchResult.depth,
            pv: searchResult.pv,
            nodes: searchResult.nodes,
            time: searchResult.time,
        };

        return searchResult.bestMove;
    };

    // 局面評価
    const evaluatePosition = (board: Board, hands: Hands, player: Player): PositionEvaluation => {
        // Use advanced evaluation function
        const evaluation = advancedEvaluatePosition(board, hands, player);

        // Return the last evaluation if available to provide search information
        if (lastEvaluation) {
            return {
                score: evaluation.total,
                depth: lastEvaluation.depth,
                pv: lastEvaluation.pv,
                nodes: lastEvaluation.nodes,
                time: lastEvaluation.time,
            };
        }

        return {
            score: evaluation.total,
            depth: 0,
            pv: [],
            nodes: 0,
            time: 0,
        };
    };

    // 停止
    const stop = (): void => {
        iterativeSearch.stop();
    };

    // 最後の評価を取得
    const getLastEvaluation = (): PositionEvaluation => {
        return (
            lastEvaluation || {
                score: 0,
                depth: 0,
                pv: [],
                nodes: 0,
                time: 0,
            }
        );
    };

    return {
        setDifficulty,
        loadOpeningBook,
        getConfig,
        setConfig,
        getSearchDepth,
        calculateBestMove,
        evaluatePosition,
        generateAllLegalMoves,
        stop,
        getLastEvaluation,
    };
}

// 後方互換性のため、クラス風のエクスポート
export const AIEngine = createAIEngine;
