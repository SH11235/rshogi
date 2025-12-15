import type { PromotionDecision } from "../types";

/**
 * 指定された移動が成れるかを判定する
 *
 * @param legalMoves - 合法手のセット（例: Set(['7g7f', '7g7f+', '2g2f'])）
 * @param from - 移動元マス（例: '7g'）
 * @param to - 移動先マス（例: '7f'）
 * @returns 成り判定の結果
 *
 * @example
 * ```typescript
 * const legalMoves = new Set(['7g7f', '7g7f+']);
 * determinePromotion(legalMoves, '7g', '7f'); // => 'optional'
 * ```
 *
 * @example
 * ```typescript
 * const legalMoves = new Set(['2c2b+']);
 * determinePromotion(legalMoves, '2c', '2b'); // => 'forced'
 * ```
 */
export function determinePromotion(
    legalMoves: Set<string>,
    from: string,
    to: string,
): PromotionDecision {
    const baseMove = `${from}${to}`;
    const promoteMove = `${baseMove}+`;

    const hasBase = legalMoves.has(baseMove);
    const hasPromote = legalMoves.has(promoteMove);

    if (hasBase && hasPromote) {
        return "optional"; // 両方存在 → 任意成り
    }
    if (hasPromote) {
        return "forced"; // 成りのみ存在 → 強制成り
    }
    return "none"; // 成れない
}
