import type { Move } from "../model/move";
import type { PieceType } from "../model/piece";
import type { Column, Row, Square } from "../model/square";

// '' は「1 始まりの行番号」と配列インデックス（0 始まり）を合わせるためのダミー要素
const kanjiRow = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"] as const;
type RowKanji = (typeof kanjiRow)[number];

/** 盤→棋譜文字列 (例: {row:7,column:5} → 5三) */
export const toKifuString = ({ row, column }: Square): string => `${column}${kanjiRow[10 - row]}`; // 10-row は実行時のみ

/** 棋譜文字列→盤（不正入力は Error） */
export const fromKifuString = (s: string): Square => {
    if (!/^[1-9][一二三四五六七八九]$/.test(s)) throw new Error(`Invalid kifu: ${s}`);

    const column = Number(s[0]) as Column;
    const rowKanji = s[1] as RowKanji;
    const rowIndex = kanjiRow.indexOf(rowKanji); // 1…9
    const row = (10 - rowIndex) as Row;

    return { row, column };
};

/**
 * 駒の種類を日本語に変換
 */
export function pieceTypeToKanji(pieceType: PieceType): string {
    const pieceMap: Record<PieceType, string> = {
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
    return pieceMap[pieceType];
}

/**
 * 日本語の駒名から駒の種類に変換
 */
export function kanjiToPieceType(kanji: string): PieceType | null {
    const kanjiMap: Record<string, PieceType> = {
        歩: "pawn",
        香: "lance",
        桂: "knight",
        銀: "silver",
        金: "gold",
        角: "bishop",
        飛: "rook",
        王: "king",
        玉: "gyoku",
    };
    return kanjiMap[kanji] || null;
}

/**
 * 数字を漢数字に変換 (1-9)
 */
export function numberToKanji(num: number): string {
    const kanjiNumbers = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    return kanjiNumbers[num] || num.toString();
}

/**
 * 漢数字を数字に変換
 */
export function kanjiToNumber(kanji: string): number | null {
    const kanjiToNumberMap: Record<string, number> = {
        一: 1,
        二: 2,
        三: 3,
        四: 4,
        五: 5,
        六: 6,
        七: 7,
        八: 8,
        九: 9,
    };
    return kanjiToNumberMap[kanji] || null;
}

/**
 * 全角数字を半角数字に変換
 */
export function fullWidthToHalfWidth(fullWidth: string): number | null {
    const fullWidthMap: Record<string, number> = {
        "１": 1,
        "２": 2,
        "３": 3,
        "４": 4,
        "５": 5,
        "６": 6,
        "７": 7,
        "８": 8,
        "９": 9,
    };
    return fullWidthMap[fullWidth] || null;
}

import type { MoveNotation } from "../model/notation";

/**
 * 手を日本語記法に変換する関数
 * @param move 変換する手
 * @param moveNumber 手番号
 * @returns 日本語記法の文字列 (例: "☗1. 歩7六")
 */
export function formatMove(move: Move, moveNumber: number): MoveNotation {
    const isBlack = move.piece.owner === "black";
    const prefix = isBlack ? "☗" : "☖";

    if (move.type === "drop") {
        const pieceChar = pieceTypeToKanji(move.piece.type);
        const rowKanji = numberToKanji(move.to.row);
        return `${prefix}${moveNumber}. ${pieceChar}打${move.to.column}${rowKanji}` as MoveNotation;
    }

    const pieceChar = pieceTypeToKanji(move.piece.type);
    const promotion = move.promote ? "成" : "";
    const capture = move.captured ? "x" : "";
    const rowKanji = numberToKanji(move.to.row);

    return `${prefix}${moveNumber}. ${pieceChar}${capture}${move.to.column}${rowKanji}${promotion}` as MoveNotation;
}
