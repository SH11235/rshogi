import type { Move } from "../domain/model/move";
import type { Piece, PieceType } from "../domain/model/piece";
import type { Column, Row, Square } from "../domain/model/square";

/**
 * YaneuraOu形式の行をパース
 * 例: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1 7g7f 34000 32 2"
 */
export function parseLine(line: string): {
    sfen: string;
    bestMove: string;
    nextMove: string;
    value: number;
    depth: number;
    num: number;
} | null {
    const parts = line.trim().split(" ");
    if (parts.length < 8) return null;

    // SFEN部分（局面表現）
    const sfen = parts.slice(0, 4).join(" ");

    // 最善手、次善手、評価値、深さ、数
    const bestMove = parts[4];
    const nextMove = parts[5] || "";
    const value = Number.parseInt(parts[6]) || 0;
    const depth = Number.parseInt(parts[7]) || 0;
    const num = Number.parseInt(parts[8]) || 0;

    return {
        sfen,
        bestMove,
        nextMove,
        value,
        depth,
        num,
    };
}

/**
 * 座標文字列をSquareに変換
 * 例: "7g" -> {row: 7, column: 7}
 */
function parseSquare(squareStr: string): Square | null {
    if (squareStr.length !== 2) return null;

    const col = Number.parseInt(squareStr[0]);
    const rowChar = squareStr[1];
    const row = rowChar.charCodeAt(0) - 96; // 'a' = 1, 'b' = 2, ...

    if (col < 1 || col > 9 || row < 1 || row > 9) return null;

    return {
        row: row as Row,
        column: col as Column,
    };
}

/**
 * 駒文字をPieceTypeに変換
 */
function charToPieceType(char: string): PieceType | null {
    const map: Record<string, PieceType> = {
        P: "pawn",
        L: "lance",
        N: "knight",
        S: "silver",
        G: "gold",
        B: "bishop",
        R: "rook",
        K: "king",
    };
    return map[char.toUpperCase()] || null;
}

/**
 * 移動をMove型に変換
 * 例: "7g7f" -> { type: "move", from: {row: 7, column: 7}, to: {row: 7, column: 6}, ... }
 */
export function convertMove(moveStr: string): Move | null {
    if (!moveStr || moveStr === "none") return null;

    // 打つ手の処理（例: "P*5e"）
    if (moveStr.includes("*")) {
        const [pieceStr, toStr] = moveStr.split("*");
        const pieceType = charToPieceType(pieceStr);
        if (!pieceType) return null;

        const to = parseSquare(toStr);
        if (!to) return null;

        return {
            type: "drop",
            to,
            piece: {
                type: pieceType,
                owner: "black", // 手番は後で設定
                promoted: false,
            },
        };
    }

    // 通常の移動（例: "7g7f"）
    if (moveStr.length >= 4) {
        const fromStr = moveStr.substring(0, 2);
        const toStr = moveStr.substring(2, 4);
        const promote = moveStr.endsWith("+");

        const from = parseSquare(fromStr);
        const to = parseSquare(toStr);
        if (!from || !to) return null;

        return {
            type: "move",
            from,
            to,
            piece: null as unknown as Piece, // 後で設定
            promote,
            captured: null,
        };
    }

    return null;
}

/**
 * 評価値を勝率に変換（YaneuraOu方式）
 */
export function valueToWinRate(value: number): number {
    // Stockfishの評価値を勝率に変換する式
    return 1 / (1 + Math.exp(-value / 600));
}

/**
 * 手の文字列表現を生成（日本語）
 */
export function getMoveString(moveStr: string): string {
    const move = convertMove(moveStr);
    if (!move) return moveStr;

    if (move.type === "drop") {
        const pieceNames: Record<PieceType, string> = {
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
        return `${move.to.column}${move.to.row}${pieceNames[move.piece.type]}打`;
    }

    // 通常の移動
    const fromStr = `${move.from.column}${move.from.row}`;
    const toStr = `${move.to.column}${move.to.row}`;
    const promoteStr = move.promote ? "成" : "";
    return `${fromStr}-${toStr}${promoteStr}`;
}
