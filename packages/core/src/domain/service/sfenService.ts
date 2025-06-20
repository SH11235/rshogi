import { modernInitialBoard } from "../initialBoard";
import type { Board } from "../model/board";
import { getPiece } from "../model/board";
import type { Piece, PieceType, Player } from "../model/piece";
import { convertToJapaneseName } from "../model/piece";
import type { Square } from "../model/square";
import { initialHands } from "./moveService";
import type { Hands } from "./moveService";

/**
 * SFEN (Shogi Forsyth-Edwards Notation) 形式のサポート
 */

// SFEN文字と駒タイプのマッピング
const SFEN_TO_PIECE: Record<string, { type: PieceType; promoted: boolean }> = {
    // 先手（大文字）
    P: { type: "pawn", promoted: false },
    L: { type: "lance", promoted: false },
    N: { type: "knight", promoted: false },
    S: { type: "silver", promoted: false },
    G: { type: "gold", promoted: false },
    B: { type: "bishop", promoted: false },
    R: { type: "rook", promoted: false },
    K: { type: "king", promoted: false },
    // 成駒（先手）
    "+P": { type: "pawn", promoted: true },
    "+L": { type: "lance", promoted: true },
    "+N": { type: "knight", promoted: true },
    "+S": { type: "silver", promoted: true },
    "+B": { type: "bishop", promoted: true },
    "+R": { type: "rook", promoted: true },
    // 後手（小文字）
    p: { type: "pawn", promoted: false },
    l: { type: "lance", promoted: false },
    n: { type: "knight", promoted: false },
    s: { type: "silver", promoted: false },
    g: { type: "gold", promoted: false },
    b: { type: "bishop", promoted: false },
    r: { type: "rook", promoted: false },
    k: { type: "king", promoted: false },
    // 成駒（後手）
    "+p": { type: "pawn", promoted: true },
    "+l": { type: "lance", promoted: true },
    "+n": { type: "knight", promoted: true },
    "+s": { type: "silver", promoted: true },
    "+b": { type: "bishop", promoted: true },
    "+r": { type: "rook", promoted: true },
};

const PIECE_TO_SFEN: Record<PieceType, string> = {
    pawn: "P",
    lance: "L",
    knight: "N",
    silver: "S",
    gold: "G",
    bishop: "B",
    rook: "R",
    king: "K",
    gyoku: "K",
};

// 駒台の駒タイプ記号
const HAND_PIECE_TYPES: PieceType[] = [
    "rook",
    "bishop",
    "gold",
    "silver",
    "knight",
    "lance",
    "pawn",
];

export interface SfenPosition {
    board: Board;
    hands: Hands;
    currentPlayer: Player;
    moveNumber: number;
}

/**
 * 盤面をSFEN形式の文字列に変換
 */
function boardToSfen(board: Board): string {
    const rows: string[] = [];

    for (let row = 1; row <= 9; row++) {
        let rowStr = "";
        let emptyCount = 0;

        for (let col = 9; col >= 1; col--) {
            const square: Square = {
                row: row as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                column: col as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
            };
            const piece = getPiece(board, square);

            if (piece) {
                if (emptyCount > 0) {
                    rowStr += emptyCount;
                    emptyCount = 0;
                }

                const pieceChar = PIECE_TO_SFEN[piece.type];
                const char = piece.owner === "black" ? pieceChar : pieceChar.toLowerCase();
                rowStr += piece.promoted ? `+${char}` : char;
            } else {
                emptyCount++;
            }
        }

        if (emptyCount > 0) {
            rowStr += emptyCount;
        }

        rows.push(rowStr);
    }

    return rows.join("/");
}

/**
 * 持ち駒をSFEN形式の文字列に変換
 */
function handsToSfen(hands: Hands): string {
    if (
        Object.values(hands.black).every((count) => count === 0) &&
        Object.values(hands.white).every((count) => count === 0)
    ) {
        return "-";
    }

    let result = "";

    // 駒種類ごとに、先手→後手の順で処理（SFEN標準）
    for (const pieceType of HAND_PIECE_TYPES) {
        const japaneseName = convertToJapaneseName(pieceType);

        // 先手の駒
        const blackCount = hands.black[japaneseName];
        if (blackCount > 0) {
            if (blackCount > 1) {
                result += blackCount;
            }
            result += PIECE_TO_SFEN[pieceType];
        }

        // 後手の駒（同じ駒種類）
        const whiteCount = hands.white[japaneseName];
        if (whiteCount > 0) {
            if (whiteCount > 1) {
                result += whiteCount;
            }
            result += PIECE_TO_SFEN[pieceType].toLowerCase();
        }
    }

    return result || "-";
}

/**
 * 現在の局面をSFEN形式でエクスポート
 */
export function exportToSfen(
    board: Board,
    hands: Hands,
    currentPlayer: Player,
    moveNumber: number,
): string {
    const boardStr = boardToSfen(board);
    const handsStr = handsToSfen(hands);
    const turnStr = currentPlayer === "black" ? "b" : "w";

    return `sfen ${boardStr} ${turnStr} ${handsStr} ${moveNumber}`;
}

/**
 * SFEN形式の文字列から盤面を解析
 */
function parseSfenBoard(sfenBoard: string): Board {
    const board: Record<string, Piece> = {};
    const rows = sfenBoard.split("/");

    if (rows.length !== 9) {
        throw new Error(`無効なSFEN盤面: 9行必要ですが${rows.length}行しかありません`);
    }

    for (let rowIndex = 0; rowIndex < 9; rowIndex++) {
        const row = rowIndex + 1;
        const rowStr = rows[rowIndex];
        let col = 9;
        let i = 0;

        while (i < rowStr.length && col >= 1) {
            const char = rowStr[i];

            // 数字の場合は空マスをスキップ
            if (/[1-9]/.test(char)) {
                const emptyCount = Number.parseInt(char, 10);
                col -= emptyCount;
                i++;
                continue;
            }

            // 成駒の処理
            if (char === "+") {
                if (i + 1 >= rowStr.length) {
                    throw new Error("無効なSFEN: +の後に文字がありません");
                }
                const pieceChar = rowStr[i + 1];
                const pieceKey = `+${pieceChar}`;
                const pieceInfo = SFEN_TO_PIECE[pieceKey];

                if (!pieceInfo) {
                    throw new Error(`無効なSFEN駒: ${pieceKey}`);
                }

                const square: Square = {
                    row: row as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                    column: col as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                };

                const piece: Piece = {
                    type: pieceInfo.type,
                    owner: pieceChar === pieceChar.toUpperCase() ? "black" : "white",
                    promoted: pieceInfo.promoted,
                };

                board[`${square.row}${square.column}`] = piece;
                col--;
                i += 2;
            } else {
                // 通常の駒
                const pieceInfo = SFEN_TO_PIECE[char];
                if (!pieceInfo) {
                    throw new Error(`無効なSFEN駒: ${char}`);
                }

                const square: Square = {
                    row: row as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                    column: col as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                };

                const piece: Piece = {
                    type: pieceInfo.type,
                    owner: char === char.toUpperCase() ? "black" : "white",
                    promoted: pieceInfo.promoted,
                };

                board[`${square.row}${square.column}`] = piece;
                col--;
                i++;
            }
        }

        if (col !== 0) {
            throw new Error(`無効なSFEN行: ${rowStr} (列の数が合いません)`);
        }
    }

    return board as Board;
}

/**
 * SFEN形式の持ち駒文字列を解析
 */
function parseSfenHands(sfenHands: string): Hands {
    const hands = initialHands();

    if (sfenHands === "-") {
        return hands;
    }

    let i = 0;
    while (i < sfenHands.length) {
        let count = 1;

        // 数字の処理
        if (/[0-9]/.test(sfenHands[i])) {
            let numStr = "";
            while (i < sfenHands.length && /[0-9]/.test(sfenHands[i])) {
                numStr += sfenHands[i];
                i++;
            }
            count = Number.parseInt(numStr, 10);
        }

        if (i >= sfenHands.length) {
            throw new Error("無効なSFEN持ち駒: 数字の後に駒がありません");
        }

        const pieceChar = sfenHands[i];
        const isBlack = pieceChar === pieceChar.toUpperCase();
        const pieceType = Object.entries(PIECE_TO_SFEN).find(
            ([, char]) => char.toLowerCase() === pieceChar.toLowerCase(),
        )?.[0] as PieceType | undefined;

        if (!pieceType) {
            throw new Error(`無効なSFEN持ち駒: ${pieceChar}`);
        }

        const japaneseName = convertToJapaneseName(pieceType);
        if (isBlack) {
            hands.black[japaneseName] += count;
        } else {
            hands.white[japaneseName] += count;
        }

        i++;
    }

    return hands;
}

/**
 * SFEN形式の文字列から局面を解析
 */
export function parseSfen(sfen: string): SfenPosition {
    const parts = sfen.trim().split(/\s+/);

    // "sfen" プレフィックスの処理
    if (parts[0] === "sfen") {
        parts.shift();
    }

    if (parts.length < 4) {
        throw new Error("無効なSFEN: 最低4つの要素が必要です (盤面 手番 持ち駒 手数)");
    }

    const [boardStr, turnStr, handsStr, moveNumberStr] = parts;

    // 盤面の解析
    const board = parseSfenBoard(boardStr);

    // 手番の解析
    if (turnStr !== "b" && turnStr !== "w") {
        throw new Error(`無効な手番: ${turnStr}`);
    }
    const currentPlayer: Player = turnStr === "b" ? "black" : "white";

    // 持ち駒の解析
    const hands = parseSfenHands(handsStr);

    // 手数の解析
    const moveNumber = Number.parseInt(moveNumberStr, 10);
    if (Number.isNaN(moveNumber) || moveNumber < 1) {
        throw new Error(`無効な手数: ${moveNumberStr}`);
    }

    return {
        board,
        hands,
        currentPlayer,
        moveNumber,
    };
}

/**
 * SFEN形式のバリデーション
 */
export function validateSfenFormat(sfen: string): { valid: boolean; error?: string } {
    try {
        parseSfen(sfen);
        return { valid: true };
    } catch (error) {
        return {
            valid: false,
            error: error instanceof Error ? error.message : "不明なエラー",
        };
    }
}

/**
 * 平手初期局面のSFEN
 */
export const INITIAL_SFEN = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

/**
 * 平手初期局面かどうかを判定
 */
export function isInitialPosition(board: Board, hands: Hands): boolean {
    const initialSfen = exportToSfen(modernInitialBoard, initialHands(), "black", 1);
    const currentSfen = exportToSfen(board, hands, "black", 1);

    // 手番と手数を除いて比較
    const initialParts = initialSfen.split(" ");
    const currentParts = currentSfen.split(" ");

    return (
        initialParts[1] === currentParts[1] && // 盤面
        initialParts[3] === currentParts[3] // 持ち駒
    );
}
