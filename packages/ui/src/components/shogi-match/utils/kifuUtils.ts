/**
 * USI/SFEN 形式の入力文字列を解析して指し手の配列を取得する
 *
 * @param raw - USI/SFEN 形式の文字列（例: "startpos moves 7g7f 3c3d" または "7g7f 3c3d"）
 * @returns 指し手の配列。空文字列の場合は空配列を返す
 *
 * @example
 * ```typescript
 * parseUsiInput("startpos moves 7g7f 3c3d");
 * // => ["7g7f", "3c3d"]
 *
 * parseUsiInput("7g7f 3c3d 2g2f");
 * // => ["7g7f", "3c3d", "2g2f"]
 *
 * parseUsiInput("");
 * // => []
 * ```
 */
export function parseUsiInput(raw: string): string[] {
    const trimmed = raw.trim();
    if (!trimmed) return [];

    // "moves" キーワードが含まれる場合は、その後の部分のみを抽出
    const movesIndex = trimmed.search(/\bmoves\b/);
    if (movesIndex !== -1) {
        const afterMoves = trimmed.slice(movesIndex + "moves".length).trim();
        return afterMoves ? afterMoves.split(/\s+/) : [];
    }

    // "moves" がない場合で startpos/sfen 系は手なしとみなす
    const tokens = trimmed.split(/\s+/);
    if (tokens[0] === "position") {
        const second = tokens[1];
        if (second === "startpos" || second === "sfen") {
            return [];
        }
    }
    if (tokens[0] === "startpos" || tokens[0] === "sfen") {
        return [];
    }

    // その他はそのまま空白区切りで解釈
    return trimmed.split(/\s+/);
}
