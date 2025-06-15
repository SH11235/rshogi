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
