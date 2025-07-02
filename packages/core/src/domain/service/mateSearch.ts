import type { Board } from "../model/board";
import { getPiece } from "../model/board";
import type { Move, NormalMove } from "../model/move";
import type { Player } from "../model/piece";
import { canPromote } from "../model/piece";
import type { Column, Row, Square } from "../model/square";
import { isCheckmate, isInCheck } from "./checkmate";
import { generateAllDropMoves } from "./generateDropMoves";
import { generateLegalMoves } from "./legalMoves";
import type { Hands } from "./moveService";
import { applyMove } from "./moveService";

export interface MateSearchResult {
    isMate: boolean;
    moves: Move[];
    nodeCount: number;
    elapsedMs: number;
}

export interface MateSearchOptions {
    maxDepth: number;
    timeout?: number;
}

/**
 * プレイヤーの全ての合法手を生成
 */
export function getAllLegalMoves(board: Board, hands: Hands, player: Player): Move[] {
    const moves: Move[] = [];

    // 盤上の駒の移動を生成
    for (let row = 1; row <= 9; row++) {
        for (let col = 1; col <= 9; col++) {
            const square: Square = { row: row as Row, column: col as Column };
            const piece = getPiece(board, square);
            if (piece && piece.owner === player) {
                const legalSquares = generateLegalMoves(board, hands, square, player);
                for (const to of legalSquares) {
                    const capturedPiece = getPiece(board, to);
                    // 通常の移動
                    moves.push({
                        type: "move",
                        from: square,
                        to,
                        piece,
                        promote: false,
                        captured: capturedPiece,
                    } as NormalMove);

                    // 成りが可能な場合
                    const canPromotePiece = canPromote(piece);
                    const inPromotionZone = player === "black" ? to.row <= 3 : to.row >= 7;
                    if (canPromotePiece && inPromotionZone) {
                        moves.push({
                            type: "move",
                            from: square,
                            to,
                            piece,
                            promote: true,
                            captured: capturedPiece,
                        } as NormalMove);
                    }
                }
            }
        }
    }

    // 持ち駒の打ち手を生成
    const dropMoves = generateAllDropMoves(board, hands, player);
    for (const drop of dropMoves) {
        try {
            // 合法性チェック
            const result = applyMove(board, hands, player, drop);
            if (!isInCheck(result.board, player)) {
                moves.push(drop);
            }
        } catch {
            // 非合法な手はスキップ
        }
    }

    return moves;
}

/**
 * 詰み探索サービス
 * 1手詰めから始めて、指定された深さまで詰みを探索
 */
export class MateSearchService {
    private nodeCount = 0;
    private startTime = 0;
    private timeout = 30000; // デフォルト30秒

    /**
     * 詰み探索を実行
     */
    public search(
        board: Board,
        hands: Hands,
        attacker: Player,
        options: MateSearchOptions,
    ): MateSearchResult {
        this.nodeCount = 0;
        this.startTime = Date.now();
        this.timeout = options.timeout || 30000;

        const result: MateSearchResult = {
            isMate: false,
            moves: [],
            nodeCount: 0,
            elapsedMs: 0,
        };

        try {
            // 奇数深さ（1手詰め、3手詰め、5手詰め...）で探索
            for (let depth = 1; depth <= options.maxDepth; depth += 2) {
                const moves: Move[] = [];
                if (this.searchMate(board, hands, attacker, depth, moves)) {
                    result.isMate = true;
                    result.moves = [...moves];
                    break;
                }

                // タイムアウトチェック
                if (this.isTimeout()) {
                    break;
                }
            }
        } finally {
            result.nodeCount = this.nodeCount;
            result.elapsedMs = Date.now() - this.startTime;
        }

        return result;
    }

    /**
     * 指定された深さで詰みを探索（攻め方の手番）
     */
    private searchMate(
        board: Board,
        hands: Hands,
        attacker: Player,
        depth: number,
        moves: Move[],
    ): boolean {
        this.nodeCount++;

        // タイムアウトチェック
        if (this.isTimeout()) {
            return false;
        }

        if (depth === 1) {
            // 1手詰めの判定
            return this.searchOneMoveCheckmate(board, hands, attacker, moves);
        }

        // 攻め方の全ての合法手を生成
        const legalMoves = getAllLegalMoves(board, hands, attacker);

        for (const move of legalMoves) {
            // 手を実行
            const result = applyMove(board, hands, attacker, move);

            moves.push(move);

            // 受け方が詰んでいるか確認
            if (isCheckmate(result.board, result.hands, this.getOpponent(attacker))) {
                return true;
            }

            // 受け方の応手を探索
            if (
                this.searchDefense(
                    result.board,
                    result.hands,
                    this.getOpponent(attacker),
                    depth - 1,
                    moves,
                )
            ) {
                return true;
            }

            // 手を戻す
            moves.pop();
        }

        return false;
    }

    /**
     * 受け方の応手を探索（受け方の手番）
     */
    private searchDefense(
        board: Board,
        hands: Hands,
        defender: Player,
        depth: number,
        moves: Move[],
    ): boolean {
        this.nodeCount++;

        // タイムアウトチェック
        if (this.isTimeout()) {
            return false;
        }

        // 受け方の全ての合法手を生成
        const legalMoves = getAllLegalMoves(board, hands, defender);

        // 合法手がない場合は詰み
        if (legalMoves.length === 0) {
            return true;
        }

        // 全ての応手に対して詰みがあるかチェック
        for (const move of legalMoves) {
            const result = applyMove(board, hands, defender, move);

            moves.push(move);

            // 攻め方の次の手を探索
            const isMate = this.searchMate(
                result.board,
                result.hands,
                this.getOpponent(defender),
                depth - 1,
                moves,
            );

            // 手を戻す
            moves.pop();

            // 詰まない応手が見つかった
            if (!isMate) {
                return false;
            }
        }

        // 全ての応手で詰む
        return true;
    }

    /**
     * 1手詰めを探索
     */
    private searchOneMoveCheckmate(
        board: Board,
        hands: Hands,
        attacker: Player,
        moves: Move[],
    ): boolean {
        const legalMoves = getAllLegalMoves(board, hands, attacker);

        for (const move of legalMoves) {
            const result = applyMove(board, hands, attacker, move);

            // 相手が詰んでいるかチェック
            if (isCheckmate(result.board, result.hands, this.getOpponent(attacker))) {
                moves.push(move);
                return true;
            }
        }

        return false;
    }

    /**
     * 相手プレイヤーを取得
     */
    private getOpponent(player: Player): Player {
        return player === "black" ? "white" : "black";
    }

    /**
     * タイムアウトチェック
     */
    private isTimeout(): boolean {
        return Date.now() - this.startTime > this.timeout;
    }
}

/**
 * 簡易的な詰み探索（1手詰めのみ）
 */
export function findOneMoveCheckmate(board: Board, hands: Hands, attacker: Player): Move | null {
    const service = new MateSearchService();
    const result = service.search(board, hands, attacker, { maxDepth: 1 });

    if (result.isMate && result.moves.length > 0) {
        return result.moves[0];
    }

    return null;
}

/**
 * 詰み探索（指定手数まで）
 */
export function findCheckmate(
    board: Board,
    hands: Hands,
    attacker: Player,
    maxMoves = 7,
): MateSearchResult {
    const service = new MateSearchService();
    return service.search(board, hands, attacker, { maxDepth: maxMoves });
}
