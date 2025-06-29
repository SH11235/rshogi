import type { Board } from "../model/board";
import type { PieceType, Player } from "../model/piece";
import type { Square, SquareKey } from "../model/square";
import { generateLegalDropMovesForPiece, generateLegalMoves } from "./legalMoves";
import type { Hands } from "./moveService";

/**
 * 受信した指し手が合法かどうかを検証する
 */
export function validateReceivedMove(
    board: Board,
    hands: Hands,
    currentPlayer: Player,
    moveData: {
        from?: SquareKey;
        to: SquareKey;
        promote?: boolean;
        drop?: PieceType;
    },
): { valid: boolean; error?: string } {
    try {
        // 座標の検証
        if (!moveData.to || typeof moveData.to !== "string" || moveData.to.length !== 2) {
            return { valid: false, error: "移動先の座標が不正です" };
        }

        const toRow = Number.parseInt(moveData.to[0]);
        const toColumn = Number.parseInt(moveData.to[1]);

        if (
            Number.isNaN(toRow) ||
            Number.isNaN(toColumn) ||
            toRow < 1 ||
            toRow > 9 ||
            toColumn < 1 ||
            toColumn > 9
        ) {
            return { valid: false, error: "移動先の座標が不正です" };
        }

        const to: Square = {
            row: toRow as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
            column: toColumn as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
        };

        if (moveData.drop) {
            // 駒打ちの検証
            return validateDropMove(board, hands, currentPlayer, moveData.drop, to);
        }
        if (moveData.from && typeof moveData.from === "string") {
            // 通常の移動の検証
            if (moveData.from.length !== 2) {
                return { valid: false, error: "移動元の座標が不正です" };
            }

            const fromRow = Number.parseInt(moveData.from[0]);
            const fromColumn = Number.parseInt(moveData.from[1]);

            if (
                Number.isNaN(fromRow) ||
                Number.isNaN(fromColumn) ||
                fromRow < 1 ||
                fromRow > 9 ||
                fromColumn < 1 ||
                fromColumn > 9
            ) {
                return { valid: false, error: "移動元の座標が不正です" };
            }

            const from: Square = {
                row: fromRow as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                column: fromColumn as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
            };

            return validateNormalMove(board, hands, currentPlayer, from, to, moveData.promote);
        }
        return { valid: false, error: "移動データが不完全です" };
    } catch (error) {
        return { valid: false, error: `検証エラー: ${error}` };
    }
}

/**
 * 通常の移動を検証
 */
function validateNormalMove(
    board: Board,
    hands: Hands,
    currentPlayer: Player,
    from: Square,
    to: Square,
    promote?: boolean,
): { valid: boolean; error?: string } {
    const fromKey = `${from.row}${from.column}` as SquareKey;
    const piece = board[fromKey];

    // 移動元に駒が存在するか
    if (!piece) {
        return { valid: false, error: "移動元に駒が存在しません" };
    }

    // 自分の駒か
    if (piece.owner !== currentPlayer) {
        return { valid: false, error: "相手の駒は動かせません" };
    }

    // 合法手を生成
    const legalMoves = generateLegalMoves(board, hands, from, currentPlayer);

    // 移動先が合法手に含まれているか
    const isLegalMove = legalMoves.some((move) => move.row === to.row && move.column === to.column);

    if (!isLegalMove) {
        return { valid: false, error: "その位置には移動できません" };
    }

    // 成りの検証
    if (promote && piece.promoted) {
        return { valid: false, error: "既に成っている駒です" };
    }

    return { valid: true };
}

/**
 * 駒打ちを検証
 */
function validateDropMove(
    board: Board,
    hands: Hands,
    currentPlayer: Player,
    pieceType: PieceType,
    to: Square,
): { valid: boolean; error?: string } {
    // 駒名マップ
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
    if (!japaneseName) {
        return { valid: false, error: "不正な駒タイプです" };
    }

    // 持ち駒を持っているか
    if (!hands[currentPlayer][japaneseName] || hands[currentPlayer][japaneseName] <= 0) {
        return { valid: false, error: "その駒を持っていません" };
    }

    // 合法な打ち場所を生成
    const legalDrops = generateLegalDropMovesForPiece(board, hands, pieceType, currentPlayer);

    // 打ち場所が合法手に含まれているか
    const isLegalDrop = legalDrops.some(
        (square) => square.row === to.row && square.column === to.column,
    );

    if (!isLegalDrop) {
        return { valid: false, error: "その位置には打てません" };
    }

    return { valid: true };
}
