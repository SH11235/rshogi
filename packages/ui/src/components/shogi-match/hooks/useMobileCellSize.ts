import { useEffect, useRef, useState } from "react";
import { MOBILE_BREAKPOINT } from "./useMediaQuery";

// =============================================================================
// 定数定義
// =============================================================================

const BOARD_CELLS = 9;
const PC_CELL_SIZE = 44;

// 横方向のマージン（盤面装飾含む）
// - MobileLayout px-2: 8px × 2 = 16px
// - ShogiBoard border: 1px × 2 = 2px
// - 左右パディング px-0.5: 2px × 2 = 4px
// - 選択リング余裕: 4px
const HORIZONTAL_MARGIN = 26;

// セルサイズの範囲
const MIN_CELL_SIZE = 28;
const MAX_CELL_SIZE = 52;

// =============================================================================
// ヘルパー関数
// =============================================================================

/**
 * 画面幅から最適なセルサイズを計算
 * 高さは考慮せず、幅のみで決定（高さはFlexboxで自動調整）
 */
function calcCellSizeFromWidth(viewportWidth: number): number {
    const availableWidth = viewportWidth - HORIZONTAL_MARGIN;
    const cellSize = Math.floor(availableWidth / BOARD_CELLS);
    return Math.max(MIN_CELL_SIZE, Math.min(cellSize, MAX_CELL_SIZE));
}

/**
 * 初期セルサイズを計算（SSR対応）
 */
function getInitialCellSize(): number {
    if (typeof window === "undefined") return PC_CELL_SIZE;
    if (window.innerWidth >= MOBILE_BREAKPOINT) return PC_CELL_SIZE;
    return calcCellSizeFromWidth(window.innerWidth);
}

// =============================================================================
// フック
// =============================================================================

/**
 * モバイル時の盤面セルサイズを計算するフック
 * 画面幅のみを考慮（高さはFlexboxに任せる）
 * PC表示時は固定値44pxを返す
 *
 * @returns セルサイズ (px)
 */
export function useMobileCellSize(): number {
    const [cellSize, setCellSize] = useState(getInitialCellSize);

    const prevSizeRef = useRef(cellSize);

    // リサイズ時の再計算
    useEffect(() => {
        if (typeof window === "undefined") return;

        const updateSize = () => {
            if (window.innerWidth >= MOBILE_BREAKPOINT) {
                if (prevSizeRef.current !== PC_CELL_SIZE) {
                    prevSizeRef.current = PC_CELL_SIZE;
                    setCellSize(PC_CELL_SIZE);
                }
                return;
            }

            const newSize = calcCellSizeFromWidth(window.innerWidth);

            if (newSize !== prevSizeRef.current) {
                prevSizeRef.current = newSize;
                setCellSize(newSize);
            }
        };

        window.addEventListener("resize", updateSize);
        return () => window.removeEventListener("resize", updateSize);
    }, []);

    return cellSize;
}
