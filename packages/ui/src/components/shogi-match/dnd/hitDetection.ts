/**
 * 将棋盤 編集モード DnD ヒットテスト
 *
 * document.elementFromPoint() を使用して DOM 要素から直接判定
 * data-square 属性（盤面マス）と data-zone 属性（持ち駒・削除）を使用
 *
 * 設計書: docs/edit-mode-dnd-design-refined.md Section 3
 */

import type { Square } from "@shogi/app-core";
import type { DropTarget } from "./types";

/**
 * スクリーン座標から盤上のマスを取得
 *
 * DOM の data-square 属性を持つ要素を探して Square を返す
 *
 * @param x - スクリーンX座標
 * @param y - スクリーンY座標
 * @returns マス、または該当なしなら null
 */
export function hitTestBoard(x: number, y: number): Square | null {
    const el = document.elementFromPoint(x, y);
    if (!el) return null;

    const squareEl = el.closest("[data-square]");
    if (!squareEl) return null;

    const square = squareEl.getAttribute("data-square");
    return square as Square | null;
}

/**
 * スクリーン座標からゾーン（持ち駒エリア/削除ゾーン）を取得
 *
 * DOM の data-zone 属性を持つ要素を探して DropTarget を返す
 * - data-zone="delete" → 削除ゾーン
 * - data-zone="hand-sente" → 先手持ち駒エリア
 * - data-zone="hand-gote" → 後手持ち駒エリア
 */
export function hitTestZones(x: number, y: number): DropTarget | null {
    const el = document.elementFromPoint(x, y);
    if (!el) return null;

    const zoneEl = el.closest("[data-zone]");
    if (!zoneEl) return null;

    const zone = zoneEl.getAttribute("data-zone");
    if (zone === "delete") {
        return { type: "delete" };
    }
    if (zone === "hand-sente") {
        return { type: "hand", owner: "sente" };
    }
    if (zone === "hand-gote") {
        return { type: "hand", owner: "gote" };
    }

    return null;
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
        const square = squareEl.getAttribute("data-square");
        if (square) {
            return { type: "board", square: square as Square };
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
