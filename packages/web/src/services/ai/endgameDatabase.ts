import type { Board, Column, Hands, Move, PieceType, Player, Row } from "shogi-core";
import {
    applyMove,
    generateLegalDropMovesForPiece,
    generateLegalMoves,
    isCheckmate,
    isInCheck,
} from "shogi-core";

// 終盤データベースのエントリー
export interface EndgameEntry {
    pattern: string; // 局面パターンの識別子
    evaluation: EndgameEvaluation;
    bestMove?: Move;
}

export interface EndgameEvaluation {
    type: "mate" | "forced_win" | "advantage";
    movesToMate?: number; // 詰みまでの手数
    score: number; // 評価値
}

// 終盤パターンの種類
export enum EndgamePattern {
    // 基本的な詰み形
    BACK_RANK_MATE = "back_rank_mate",
    GOLD_SILVER_MATE = "gold_silver_mate",
    ROOK_MATE = "rook_mate",
    LANCE_MATE = "lance_mate",

    // 必勝形
    KING_ROOK_VS_KING = "king_rook_vs_king",
    KING_TWO_GOLD_VS_KING = "king_two_gold_vs_king",

    // 駒得による優勢
    MAJOR_PIECE_ADVANTAGE = "major_piece_advantage",
}

// 終盤データベースクラス
export class EndgameDatabase {
    private patterns: Map<
        string,
        (board: Board, hands: Hands, player: Player) => EndgameEvaluation | null
    >;

    constructor() {
        this.patterns = new Map();
        this.initializePatterns();
    }

    // パターンを初期化
    private initializePatterns(): void {
        // 飛車による詰みパターン
        this.patterns.set(EndgamePattern.ROOK_MATE, this.checkRookMate.bind(this));

        // 金銀による詰みパターン
        this.patterns.set(EndgamePattern.GOLD_SILVER_MATE, this.checkGoldSilverMate.bind(this));

        // 香車による詰みパターン
        this.patterns.set(EndgamePattern.LANCE_MATE, this.checkLanceMate.bind(this));

        // 駒得による優勢判定
        this.patterns.set(
            EndgamePattern.MAJOR_PIECE_ADVANTAGE,
            this.checkMajorPieceAdvantage.bind(this),
        );
    }

    // 局面を評価
    evaluate(board: Board, hands: Hands, player: Player): EndgameEvaluation | null {
        // 王が存在しない場合は評価しない
        const playerKing = this.findKing(board, player);
        const opponent = player === "black" ? "white" : "black";
        const opponentKing = this.findKing(board, opponent);

        if (!playerKing || !opponentKing) {
            return null;
        }

        // 即詰みチェック
        if (isCheckmate(board, hands, player)) {
            return {
                type: "mate",
                movesToMate: 0,
                score: -100000, // 負け
            };
        }

        if (isCheckmate(board, hands, opponent)) {
            return {
                type: "mate",
                movesToMate: 0,
                score: 100000, // 勝ち
            };
        }

        // 各パターンをチェック
        for (const [_pattern, checkFunc] of this.patterns) {
            const result = checkFunc(board, hands, player);
            if (result) {
                return result;
            }
        }

        return null;
    }

    // 飛車による詰みパターンをチェック
    private checkRookMate(board: Board, hands: Hands, player: Player): EndgameEvaluation | null {
        const opponent = player === "black" ? "white" : "black";

        // 相手が王手されているか
        if (!isInCheck(board, opponent)) {
            return null;
        }

        // 飛車で王手しているか確認
        let rookChecking = false;
        for (const [_square, piece] of Object.entries(board)) {
            if (piece && piece.owner === player && piece.type === "rook") {
                rookChecking = true;
                break;
            }
        }

        if (!rookChecking) {
            return null;
        }

        // 相手の合法手が少ない場合、詰みに近い
        const opponentKingSquare = this.findKing(board, opponent);
        if (!opponentKingSquare) return null;

        const legalMoves = generateLegalMoves(
            board,
            hands,
            {
                row: opponentKingSquare.row as Row,
                column: opponentKingSquare.column as Column,
            },
            opponent,
        );

        if (legalMoves.length === 0) {
            return {
                type: "mate",
                movesToMate: 1,
                score: 50000,
            };
        }
        if (legalMoves.length <= 2) {
            return {
                type: "forced_win",
                movesToMate: 3,
                score: 10000,
            };
        }

        return null;
    }

    // 金銀による詰みパターンをチェック
    private checkGoldSilverMate(
        board: Board,
        _hands: Hands,
        player: Player,
    ): EndgameEvaluation | null {
        const opponent = player === "black" ? "white" : "black";

        // 相手玉の位置を確認
        const opponentKingSquare = this.findKing(board, opponent);
        if (!opponentKingSquare) return null;

        // 玉が端に追い詰められているか
        const isCorner =
            (opponentKingSquare.row === 1 || opponentKingSquare.row === 9) &&
            (opponentKingSquare.column === 1 || opponentKingSquare.column === 9);

        if (!isCorner) {
            return null;
        }

        // 金銀で囲まれているかチェック
        let goldSilverCount = 0;
        const adjacentSquares = this.getAdjacentSquares(opponentKingSquare);

        for (const square of adjacentSquares) {
            const piece = board[`${square.row}${square.column}` as keyof Board];
            if (
                piece &&
                piece.owner === player &&
                (piece.type === "gold" || piece.type === "silver" || piece.promoted)
            ) {
                goldSilverCount++;
            }
        }

        if (goldSilverCount >= 2) {
            return {
                type: "forced_win",
                movesToMate: 5,
                score: 8000,
            };
        }

        return null;
    }

    // 香車による詰みパターンをチェック
    private checkLanceMate(board: Board, hands: Hands, player: Player): EndgameEvaluation | null {
        const opponent = player === "black" ? "white" : "black";

        // 相手玉の位置を確認
        const opponentKingSquare = this.findKing(board, opponent);
        if (!opponentKingSquare) return null;

        // 玉が最終段にいるか
        const isBackRank =
            (player === "black" && opponentKingSquare.row === 1) ||
            (player === "white" && opponentKingSquare.row === 9);

        if (!isBackRank) {
            return null;
        }

        // 香車で王手可能か
        const lanceInHand = hands[player].lance > 0;
        if (lanceInHand) {
            // 玉の前に空きマスがあるか確認
            const frontRow =
                player === "black" ? opponentKingSquare.row + 1 : opponentKingSquare.row - 1;
            const frontSquare = board[`${frontRow}${opponentKingSquare.column}` as keyof Board];

            if (!frontSquare) {
                return {
                    type: "forced_win",
                    movesToMate: 3,
                    score: 9000,
                };
            }
        }

        return null;
    }

    // 駒得による優勢をチェック
    private checkMajorPieceAdvantage(
        board: Board,
        hands: Hands,
        player: Player,
    ): EndgameEvaluation | null {
        const opponent = player === "black" ? "white" : "black";

        // 駒の価値を計算
        const pieceValues: Record<string, number> = {
            pawn: 100,
            lance: 300,
            knight: 400,
            silver: 500,
            gold: 600,
            bishop: 800,
            rook: 1000,
        };

        let materialDiff = 0;

        // 盤上の駒
        for (const [_square, piece] of Object.entries(board)) {
            if (piece && piece.type !== "king" && piece.type !== "gyoku") {
                const value = pieceValues[piece.type] || 0;
                const promotedBonus = piece.promoted ? value * 0.5 : 0;

                if (piece.owner === player) {
                    materialDiff += value + promotedBonus;
                } else {
                    materialDiff -= value + promotedBonus;
                }
            }
        }

        // 持ち駒
        for (const pieceType of Object.keys(pieceValues)) {
            const playerCount =
                hands[player][pieceType as keyof (typeof hands)[typeof player]] || 0;
            const opponentCount =
                hands[opponent][pieceType as keyof (typeof hands)[typeof opponent]] || 0;
            materialDiff += (playerCount - opponentCount) * pieceValues[pieceType];
        }

        // 大きな駒得がある場合
        if (materialDiff > 1500) {
            return {
                type: "advantage",
                score: Math.min(materialDiff * 2, 20000),
            };
        }

        return null;
    }

    // 王を探す
    private findKing(board: Board, player: Player): { row: number; column: number } | null {
        for (const [square, piece] of Object.entries(board)) {
            if (
                piece &&
                piece.owner === player &&
                (piece.type === "king" || piece.type === "gyoku")
            ) {
                const row = Number.parseInt(square[0]);
                const column = Number.parseInt(square[1]);
                return { row, column };
            }
        }
        return null;
    }

    // 隣接マスを取得
    private getAdjacentSquares(square: { row: number; column: number }): {
        row: number;
        column: number;
    }[] {
        const squares: { row: number; column: number }[] = [];
        const directions = [
            [-1, -1],
            [-1, 0],
            [-1, 1],
            [0, -1],
            [0, 1],
            [1, -1],
            [1, 0],
            [1, 1],
        ];

        for (const [dr, dc] of directions) {
            const newRow = square.row + dr;
            const newCol = square.column + dc;

            if (newRow >= 1 && newRow <= 9 && newCol >= 1 && newCol <= 9) {
                squares.push({ row: newRow, column: newCol });
            }
        }

        return squares;
    }

    // 詰み探索（簡易版）
    searchMate(board: Board, hands: Hands, player: Player, _maxDepth = 3): Move | null {
        // 1手詰みをチェック
        const moves = this.generateAllMoves(board, hands, player);

        for (const move of moves) {
            const result = this.simulateMove(board, hands, player, move);
            if (
                result &&
                isCheckmate(result.board, result.hands, player === "black" ? "white" : "black")
            ) {
                return move;
            }
        }

        // より深い詰み探索は複雑なので、ここでは省略
        return null;
    }

    // すべての合法手を生成
    private generateAllMoves(board: Board, hands: Hands, player: Player): Move[] {
        const moves: Move[] = [];

        // 盤上の駒の移動
        for (const [fromSquare, piece] of Object.entries(board)) {
            if (piece && piece.owner === player) {
                const square = {
                    row: Number.parseInt(fromSquare[0]) as Row,
                    column: Number.parseInt(fromSquare[1]) as Column,
                };
                const legalSquares = generateLegalMoves(board, hands, square, player);

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
                }
            }
        }

        // 持ち駒を打つ
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

        return moves;
    }

    // 手をシミュレート
    private simulateMove(
        board: Board,
        hands: Hands,
        player: Player,
        move: Move,
    ): { board: Board; hands: Hands } | null {
        try {
            return applyMove(board, hands, player, move);
        } catch {
            return null;
        }
    }
}
