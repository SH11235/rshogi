import type { SquareNotation } from "../types";

/** 段（rank）のアルファベットから漢数字への変換マップ */
const RANK_TO_KANJI: Record<string, string> = {
    a: "一",
    b: "二",
    c: "三",
    d: "四",
    e: "五",
    f: "六",
    g: "七",
    h: "八",
    i: "九",
};

/**
 * SFEN座標を日本式表記に変換する
 * @param square SFEN座標 (例: "5e")
 * @returns 日本式表記 (例: "５五")
 */
export function formatSquareJapanese(square: string): string {
    const file = square[0]; // "5"
    const rank = square[1]; // "e"
    // 半角数字を全角数字に変換 (0x30 -> 0xFF10)
    const fileZenkaku = String.fromCharCode(file.charCodeAt(0) + 0xfee0);
    return `${fileZenkaku}${RANK_TO_KANJI[rank] ?? rank}`;
}

/**
 * 座標を指定形式でフォーマットする
 * @param square SFEN座標 (例: "5e")
 * @param notation 表示形式
 * @returns フォーマット済み文字列、または null（非表示の場合）
 */
export function formatSquare(square: string, notation: SquareNotation): string | null {
    switch (notation) {
        case "none":
            return null;
        case "sfen":
            return square;
        case "japanese":
            return formatSquareJapanese(square);
    }
}

/** 盤外ラベル: 筋（先手視点、右から左へ） */
export const FILE_LABELS = ["９", "８", "７", "６", "５", "４", "３", "２", "１"] as const;

/** 盤外ラベル: 段（先手視点、上から下へ） */
export const RANK_LABELS = ["一", "二", "三", "四", "五", "六", "七", "八", "九"] as const;

/** 盤外ラベル: 筋（後手視点、反転時） */
export const FILE_LABELS_FLIPPED = [...FILE_LABELS].reverse();

/** 盤外ラベル: 段（後手視点、反転時） */
export const RANK_LABELS_FLIPPED = [...RANK_LABELS].reverse();

/**
 * 盤面反転状態に応じたラベルを取得する
 * @param flipBoard 盤面反転フラグ
 * @returns { files: string[], ranks: string[] }
 */
export function getBoardLabels(flipBoard: boolean): {
    files: readonly string[];
    ranks: readonly string[];
} {
    return {
        files: flipBoard ? FILE_LABELS_FLIPPED : FILE_LABELS,
        ranks: flipBoard ? RANK_LABELS_FLIPPED : RANK_LABELS,
    };
}
