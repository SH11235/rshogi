import { useEffect, useRef, useState } from "react";
import { MOBILE_BREAKPOINT } from "./useMediaQuery";

// 盤面全体に必要なマージン計算:
// - MobileLayout px-2: 8px × 2 = 16px
// - ShogiBoard p-2 + border: (8px + 1px) × 2 = 18px
// - 左右パディング mx-1: 4px × 2 = 8px
// - グリッド左border: 1px
// - 余裕分: 5px
// 合計: 約48px
const MOBILE_MARGIN = 48;
const BOARD_CELLS = 9;
const PC_CELL_SIZE = 44;

/**
 * スマホ時の盤面セルサイズを計算
 * 画面幅に常に収まるサイズを返す
 * @param viewportWidth - ビューポート幅
 * @returns セルサイズ (px)
 */
function calcMobileCellSize(viewportWidth: number): number {
    const availableWidth = viewportWidth - MOBILE_MARGIN;
    return Math.floor(availableWidth / BOARD_CELLS);
}

/**
 * 初期セルサイズを計算（SSR対応）
 */
function getInitialCellSize(): number {
    if (typeof window === "undefined") return PC_CELL_SIZE;
    if (window.innerWidth >= MOBILE_BREAKPOINT) return PC_CELL_SIZE;
    return calcMobileCellSize(window.innerWidth);
}

// モジュールレベルでキャッシュ（コンポーネント間で共有）
let cachedCellSize: number | null = null;

/**
 * モバイル時の盤面セルサイズを計算するフック
 * PC表示時は固定値44pxを返す
 *
 * 改善点:
 * - モジュールレベルでキャッシュしてコンポーネント間で共有
 * - 初回計算後は安定した値を返す
 * - リサイズ時のみ再計算
 *
 * @returns セルサイズ (px)
 */
export function useMobileCellSize(): number {
    // 初期値をキャッシュから取得（存在しなければ計算）
    const [cellSize, setCellSize] = useState(() => {
        if (cachedCellSize !== null) {
            return cachedCellSize;
        }
        const size = getInitialCellSize();
        cachedCellSize = size;
        return size;
    });

    // 前回の値を保持して不要な更新を防ぐ
    const prevSizeRef = useRef(cellSize);

    useEffect(() => {
        if (typeof window === "undefined") return;

        const updateSize = () => {
            const newSize =
                window.innerWidth >= MOBILE_BREAKPOINT
                    ? PC_CELL_SIZE
                    : calcMobileCellSize(window.innerWidth);

            // 値が変わった場合のみ更新
            if (newSize !== prevSizeRef.current) {
                prevSizeRef.current = newSize;
                cachedCellSize = newSize;
                setCellSize(newSize);
            }
        };

        // リサイズイベントのみ監視（初回更新は不要、useStateの初期値で設定済み）
        window.addEventListener("resize", updateSize);
        return () => window.removeEventListener("resize", updateSize);
    }, []);

    return cellSize;
}
