import { useEffect, useState } from "react";
import { MOBILE_BREAKPOINT } from "./useMediaQuery";

const MOBILE_PADDING = 16; // 左右パディング合計
const BOARD_CELLS = 9;
const PC_CELL_SIZE = 44;
const MIN_CELL_SIZE_PLAYING = 36;
const MIN_CELL_SIZE_REVIEWING = 32;
const MAX_CELL_SIZE_REVIEWING = 40;

/**
 * スマホ時の盤面セルサイズを計算
 * @param viewportWidth - ビューポート幅
 * @param mode - ゲームモード ('playing' | 'reviewing')
 * @returns セルサイズ (px)
 */
function calcMobileCellSize(viewportWidth: number, mode: "playing" | "reviewing"): number {
    const availableWidth = viewportWidth - MOBILE_PADDING;
    const cellSize = Math.floor(availableWidth / BOARD_CELLS);

    if (mode === "playing") {
        // 対局モード: フルサイズ（持ち駒は上下配置なので横幅は盤のみ）
        return Math.max(cellSize, MIN_CELL_SIZE_PLAYING);
    }
    // 検討モード: 縮小（棋譜パネルの高さを確保）
    return Math.min(Math.max(cellSize, MIN_CELL_SIZE_REVIEWING), MAX_CELL_SIZE_REVIEWING);
}

/**
 * モバイル時の盤面セルサイズを計算するフック
 * PC表示時は固定値44pxを返す
 * @param mode - ゲームモード ('playing' | 'reviewing')
 * @returns セルサイズ (px)
 */
export function useMobileCellSize(mode: "playing" | "reviewing"): number {
    const [cellSize, setCellSize] = useState(PC_CELL_SIZE);

    useEffect(() => {
        if (typeof window === "undefined") return;

        const updateSize = () => {
            if (window.innerWidth >= MOBILE_BREAKPOINT) {
                setCellSize(PC_CELL_SIZE);
            } else {
                setCellSize(calcMobileCellSize(window.innerWidth, mode));
            }
        };

        updateSize();
        window.addEventListener("resize", updateSize);
        return () => window.removeEventListener("resize", updateSize);
    }, [mode]);

    return cellSize;
}
