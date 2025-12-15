/**
 * ミリ秒を MM:SS 形式にフォーマットする
 *
 * @param ms - ミリ秒
 * @returns MM:SS 形式の文字列
 *
 * @example
 * ```typescript
 * formatTime(125000); // => "02:05"
 * formatTime(5000);   // => "00:05"
 * formatTime(-1000);  // => "00:00" (負の値は 0 として扱う)
 * ```
 */
export function formatTime(ms: number): string {
    if (ms < 0) ms = 0;
    const totalSeconds = Math.floor(ms / 1000);
    const minutes = Math.floor(totalSeconds / 60)
        .toString()
        .padStart(2, "0");
    const seconds = (totalSeconds % 60).toString().padStart(2, "0");
    return `${minutes}:${seconds}`;
}
