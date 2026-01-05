import { useEffect, useState } from "react";
import { MOBILE_BREAKPOINT } from "./useMediaQuery";

const MOBILE_MARGIN = 48; // 左右マージン（盤面装飾 + パディング + 余裕分）
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
 * モバイル時の盤面セルサイズを計算するフック
 * PC表示時は固定値44pxを返す
 * @returns セルサイズ (px)
 */
export function useMobileCellSize(): number {
    const [cellSize, setCellSize] = useState(() => {
        // SSR対応
        if (typeof window === "undefined") return PC_CELL_SIZE;
        // PC表示時は固定値
        if (window.innerWidth >= MOBILE_BREAKPOINT) return PC_CELL_SIZE;
        // モバイル時は計算
        return calcMobileCellSize(window.innerWidth);
    });

    useEffect(() => {
        if (typeof window === "undefined") return;

        const updateSize = () => {
            if (window.innerWidth >= MOBILE_BREAKPOINT) {
                setCellSize(PC_CELL_SIZE);
            } else {
                setCellSize(calcMobileCellSize(window.innerWidth));
            }
        };

        updateSize();
        window.addEventListener("resize", updateSize);
        return () => window.removeEventListener("resize", updateSize);
    }, []);

    return cellSize;
}
