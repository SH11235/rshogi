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
    if (trimmed.includes("moves")) {
        const afterMoves = trimmed.split("moves")[1]?.trim();
        return afterMoves ? afterMoves.split(/\s+/) : [];
    }

    // "moves" がない場合は、全体を空白で分割
    return trimmed.split(/\s+/);
}
