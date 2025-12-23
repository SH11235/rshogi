/**
 * 将棋盤 編集モード DnD ヒットテスト
 *
 * 盤面は規則的な9x9グリッドなので、rect + 算出で高速判定
 * 設計書: docs/edit-mode-dnd-design-refined.md Section 3
 */

import { BOARD_FILES, BOARD_RANKS } from "@shogi/app-core";
import type { Square } from "@shogi/app-core";
import type { BoardMetrics, DropTarget, Zones } from "./types";

/**
 * rect 内かどうかを判定
 */
function inside(x: number, y: number, rect: DOMRect): boolean {
    return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}

/**
 * スクリーン座標から盤上のマスを算出
 *
 * @param x - スクリーンX座標
 * @param y - スクリーンY座標
 * @param m - 盤面のメトリクス
 * @returns マス、または盤外なら null
 */
export function hitTestBoard(x: number, y: number, m: BoardMetrics): Square | null {
    const { left, top, width, height } = m.rect;

    // 盤外チェック
    if (x < left || x > left + width || y < top || y > top + height) {
        return null;
    }

    // グリッド座標を算出
    const col = Math.floor((x - left) / m.cellW);
    const row = Math.floor((y - top) / m.cellH);

    // 境界チェック（念のため）
    if (col < 0 || col > 8 || row < 0 || row > 8) {
        return null;
    }

    // 盤の向きで file/rank を変換
    // BOARD_FILES = ["9", "8", "7", "6", "5", "4", "3", "2", "1"]
    // BOARD_RANKS = ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
    if (m.orientation === "sente") {
        // 先手視点: 左上が9a、右下が1i
        // col=0 → 9筋, col=8 → 1筋
        // row=0 → a段, row=8 → i段
        const file = BOARD_FILES[col];
        const rank = BOARD_RANKS[row];
        return `${file}${rank}` as Square;
    } else {
        // 後手視点: 左上が1i、右下が9a（反転）
        // col=0 → 1筋, col=8 → 9筋
        // row=0 → i段, row=8 → a段
        const file = BOARD_FILES[8 - col];
        const rank = BOARD_RANKS[8 - row];
        return `${file}${rank}` as Square;
    }
}

/**
 * スクリーン座標から持ち駒エリア/削除ゾーンを判定
 */
export function hitTestZones(x: number, y: number, z: Zones): DropTarget | null {
    // 削除ゾーンを最優先
    if (z.deleteRect && inside(x, y, z.deleteRect)) {
        return { type: "delete" };
    }

    // 持ち駒エリア
    if (z.senteHandRect && inside(x, y, z.senteHandRect)) {
        return { type: "hand", owner: "sente" };
    }
    if (z.goteHandRect && inside(x, y, z.goteHandRect)) {
        return { type: "hand", owner: "gote" };
    }

    return null;
}

/**
 * 最終的なドロップターゲットを決定
 *
 * 優先順位:
 * 1. 削除ゾーン（明示的）
 * 2. 盤上のマス
 * 3. 持ち駒エリア
 * 4. エリア外 → delete
 */
export function getDropTarget(
    x: number,
    y: number,
    board: BoardMetrics,
    zones: Zones,
    outsideAreaBehavior: "delete" | "cancel" = "delete",
): DropTarget | null {
    // 削除ゾーンを最優先
    const zt = hitTestZones(x, y, zones);
    if (zt?.type === "delete") {
        return zt;
    }

    // 盤上のマス
    const sq = hitTestBoard(x, y, board);
    if (sq) {
        return { type: "board", square: sq };
    }

    // 持ち駒エリア
    if (zt) {
        return zt;
    }

    // エリア外
    if (outsideAreaBehavior === "delete") {
        return { type: "delete" };
    }
    return null;
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

/**
 * 盤面のメトリクスを計算
 */
export function measureBoard(
    boardElement: HTMLElement,
    orientation: "sente" | "gote",
): BoardMetrics {
    const rect = boardElement.getBoundingClientRect();
    return {
        rect,
        cellW: rect.width / 9,
        cellH: rect.height / 9,
        orientation,
    };
}

/**
 * 各ゾーンの rect を計算
 */
export function measureZones(
    senteHandElement: HTMLElement | null,
    goteHandElement: HTMLElement | null,
    deleteZoneElement: HTMLElement | null,
): Zones {
    return {
        senteHandRect: senteHandElement?.getBoundingClientRect() ?? null,
        goteHandRect: goteHandElement?.getBoundingClientRect() ?? null,
        deleteRect: deleteZoneElement?.getBoundingClientRect() ?? null,
    };
}
