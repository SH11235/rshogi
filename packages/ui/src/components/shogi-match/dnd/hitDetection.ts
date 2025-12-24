/**
 * 将棋盤 編集モード DnD ヒットテスト
 *
 * document.elementFromPoint() を使用して DOM 要素から直接判定
 * data-square 属性（盤面マス）と data-zone 属性（持ち駒・削除）を使用
 *
 * 注意: この方式はゴーストやオーバーレイに pointer-events: none が
 * 設定されていることを前提とする。pointer-events を持つオーバーレイ
 * （クリック可能な矢印等）を追加する場合は elementsFromPoint への
 * 変更を検討すること。
 */

import type { Square } from "@shogi/app-core";
import type { DropTarget } from "./types";

/** Square 形式の正規表現: File(1-9) + Rank(a-i) */
const SQUARE_PATTERN = /^[1-9][a-i]$/;

/**
 * 文字列を Square として検証・パース
 * 不正な形式の場合は null を返す
 */
function parseSquare(value: string | null): Square | null {
    if (!value || !SQUARE_PATTERN.test(value)) {
        return null;
    }
    return value as Square;
}

/**
 * 最終的なドロップターゲットを決定
 *
 * document.elementFromPoint() を使用して DOM から直接判定
 *
 * 優先順位:
 * 1. 削除ゾーン（data-zone="delete"）
 * 2. 盤上のマス（data-square）
 * 3. 持ち駒エリア（data-zone="hand-*"）
 * 4. エリア外 → outsideAreaBehavior に従う
 */
export function getDropTarget(
    x: number,
    y: number,
    outsideAreaBehavior: "delete" | "cancel" = "delete",
): DropTarget | null {
    const el = document.elementFromPoint(x, y);
    if (!el) {
        return outsideAreaBehavior === "delete" ? { type: "delete" } : null;
    }

    // 削除ゾーンを最優先
    const zoneEl = el.closest("[data-zone]");
    if (zoneEl) {
        const zone = zoneEl.getAttribute("data-zone");
        if (zone === "delete") {
            return { type: "delete" };
        }
    }

    // 盤上のマス
    const squareEl = el.closest("[data-square]");
    if (squareEl) {
        const square = parseSquare(squareEl.getAttribute("data-square"));
        if (square) {
            return { type: "board", square };
        }
    }

    // 持ち駒エリア
    if (zoneEl) {
        const zone = zoneEl.getAttribute("data-zone");
        if (zone === "hand-sente") {
            return { type: "hand", owner: "sente" };
        }
        if (zone === "hand-gote") {
            return { type: "hand", owner: "gote" };
        }
    }

    // エリア外
    return outsideAreaBehavior === "delete" ? { type: "delete" } : null;
}

/**
 * 2つの DropTarget が等しいかを判定
 */
export function dropTargetEquals(a: DropTarget | null, b: DropTarget | null): boolean {
    if (a === b) return true;
    if (a === null || b === null) return false;
    if (a.type !== b.type) return false;

    if (a.type === "board" && b.type === "board") {
        return a.square === b.square;
    }
    if (a.type === "hand" && b.type === "hand") {
        return a.owner === b.owner;
    }
    // delete 同士
    return true;
}
