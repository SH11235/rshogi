import type { Hands, PieceType, Player, PositionState } from "@shogi/app-core";

/**
 * 持ち駒の状態をクローンする
 *
 * @param hands - クローン対象の持ち駒
 * @returns クローンされた持ち駒
 */
export function cloneHandsState(hands: Hands): Hands {
    return {
        sente: { ...hands.sente },
        gote: { ...hands.gote },
    };
}

/**
 * 持ち駒に駒を追加する（イミュータブル）
 *
 * @param hands - 元の持ち駒
 * @param owner - 駒の所有者
 * @param pieceType - 追加する駒の種類
 * @returns 新しい持ち駒の状態
 *
 * @example
 * ```typescript
 * const hands = { sente: { P: 1 }, gote: {} };
 * const newHands = addToHand(hands, "sente", "P");
 * // => { sente: { P: 2 }, gote: {} }
 * ```
 */
export function addToHand(hands: Hands, owner: Player, pieceType: PieceType): Hands {
    const next = cloneHandsState(hands);
    const current = next[owner][pieceType] ?? 0;
    next[owner][pieceType] = current + 1;
    return next;
}

/**
 * 持ち駒から駒を消費する（イミュータブル）
 *
 * @param hands - 元の持ち駒
 * @param owner - 駒の所有者
 * @param pieceType - 消費する駒の種類
 * @returns 新しい持ち駒の状態。駒が不足している場合は null
 *
 * @example
 * ```typescript
 * const hands = { sente: { P: 2 }, gote: {} };
 * const newHands = consumeFromHand(hands, "sente", "P");
 * // => { sente: { P: 1 }, gote: {} }
 *
 * const emptyHands = consumeFromHand(hands, "sente", "R");
 * // => null (駒が不足)
 * ```
 */
export function consumeFromHand(hands: Hands, owner: Player, pieceType: PieceType): Hands | null {
    const next = cloneHandsState(hands);
    const current = next[owner][pieceType] ?? 0;
    if (current <= 0) return null;
    if (current === 1) {
        delete next[owner][pieceType];
    } else {
        next[owner][pieceType] = current - 1;
    }
    return next;
}

/**
 * 局面内の全駒数をカウントする（盤上＋持ち駒）
 *
 * @param position - カウント対象の局面
 * @returns 各プレイヤーごとの駒種類別カウント
 *
 * @example
 * ```typescript
 * const position = { board: {...}, hands: {...}, turn: "sente", ply: 1 };
 * const counts = countPieces(position);
 * // => { sente: { K: 1, R: 1, ... }, gote: { K: 1, R: 1, ... } }
 * ```
 */
export function countPieces(position: PositionState): Record<Player, Record<PieceType, number>> {
    const counts: Record<Player, Record<PieceType, number>> = {
        sente: { K: 0, R: 0, B: 0, G: 0, S: 0, N: 0, L: 0, P: 0 },
        gote: { K: 0, R: 0, B: 0, G: 0, S: 0, N: 0, L: 0, P: 0 },
    };

    // 盤上の駒をカウント
    for (const piece of Object.values(position.board)) {
        if (!piece) continue;
        counts[piece.owner][piece.type] += 1;
    }

    // 持ち駒をカウント
    for (const owner of ["sente", "gote"] as Player[]) {
        const hand = position.hands[owner];
        for (const key of Object.keys(hand) as PieceType[]) {
            counts[owner][key] += hand[key] ?? 0;
        }
    }

    return counts;
}
