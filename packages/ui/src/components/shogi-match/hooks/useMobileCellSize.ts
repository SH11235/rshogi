import { useEffect, useRef, useState } from "react";
import { MOBILE_BREAKPOINT } from "./useMediaQuery";
import type { GameMode } from "../types";

// =============================================================================
// 定数定義
// =============================================================================

const BOARD_CELLS = 9;
const PC_CELL_SIZE = 44;

// 横方向のマージン（盤面装飾含む）
// - MobileLayout px-2: 8px × 2 = 16px
// - ShogiBoard p-2 + border: (8px + 1px) × 2 = 18px
// - 左右パディング mx-1: 4px × 2 = 8px
// - 選択リング余裕: 4px
const HORIZONTAL_MARGIN = 46;

// 縦方向のUI高さ（盤面以外の要素）
const UI_HEIGHTS = {
    // 共通要素
    statusBar: 40, // ステータス行
    handPieceCompact: 24, // 持ち駒エリア（編集モード時、compact min-h-[24px]）
    handPieceMedium: 32, // 持ち駒エリア（対局/検討モード時、medium min-h-[32px]）
    boardDecoration: 40, // 盤面装飾（パディング、ボーダー、ラベル余白）
    safeArea: 20, // 下部セーフエリア

    // モード別要素
    clock: 40, // クロック表示（対局時）
    playingUI: 100, // 対局モードUI（棋譜バー + 停止ボタン）
    reviewUI: 200, // 検討モードUI（評価値グラフ + ナビゲーション + 棋譜バー + ボタン）
    prepareUI: 72, // 対局準備UI（開始ボタンのみ）
    editUI: 80, // 編集モードUI（テキスト + ボタン）
} as const;

// =============================================================================
// ヘルパー関数
// =============================================================================

/**
 * モードに応じたUI高さの合計を計算
 */
function calcUIHeight(gameMode: GameMode, hasKifuMoves: boolean): number {
    // 編集モード時はcompact、それ以外はmediumサイズの持ち駒
    const handPieceHeight =
        gameMode === "editing" ? UI_HEIGHTS.handPieceCompact : UI_HEIGHTS.handPieceMedium;

    const base =
        UI_HEIGHTS.statusBar +
        handPieceHeight * 2 +
        UI_HEIGHTS.boardDecoration +
        UI_HEIGHTS.safeArea;

    switch (gameMode) {
        case "playing":
            return base + UI_HEIGHTS.clock + UI_HEIGHTS.playingUI;
        case "reviewing":
            // 棋譜がない場合は対局準備モード
            return base + (hasKifuMoves ? UI_HEIGHTS.reviewUI : UI_HEIGHTS.prepareUI);
        case "editing":
            return base + UI_HEIGHTS.editUI;
        default:
            return base + UI_HEIGHTS.reviewUI; // 最大ケースをデフォルトに
    }
}

/**
 * 幅と高さから最適なセルサイズを計算
 */
function calcOptimalCellSize(
    viewportWidth: number,
    viewportHeight: number,
    gameMode: GameMode,
    hasKifuMoves: boolean,
): number {
    // 幅から計算
    const availableWidth = viewportWidth - HORIZONTAL_MARGIN;
    const cellSizeFromWidth = Math.floor(availableWidth / BOARD_CELLS);

    // 高さから計算
    const uiHeight = calcUIHeight(gameMode, hasKifuMoves);
    const availableHeight = viewportHeight - uiHeight;
    const cellSizeFromHeight = Math.floor(availableHeight / BOARD_CELLS);

    // 小さい方を採用（画面に収まるように）
    return Math.max(28, Math.min(cellSizeFromWidth, cellSizeFromHeight));
}

/**
 * 初期セルサイズを計算（SSR対応）
 */
function getInitialCellSize(gameMode: GameMode, hasKifuMoves: boolean): number {
    if (typeof window === "undefined") return PC_CELL_SIZE;
    if (window.innerWidth >= MOBILE_BREAKPOINT) return PC_CELL_SIZE;
    return calcOptimalCellSize(window.innerWidth, window.innerHeight, gameMode, hasKifuMoves);
}

// =============================================================================
// フック
// =============================================================================

export interface UseMobileCellSizeOptions {
    /** 現在のゲームモード */
    gameMode?: GameMode;
    /** 棋譜があるかどうか（検討モードのUI判定用） */
    hasKifuMoves?: boolean;
}

/**
 * モバイル時の盤面セルサイズを計算するフック
 * 画面幅と画面高さの両方を考慮し、スクロールなしで収まるサイズを返す
 * PC表示時は固定値44pxを返す
 *
 * @param options - オプション
 * @returns セルサイズ (px)
 */
export function useMobileCellSize(options: UseMobileCellSizeOptions = {}): number {
    const { gameMode = "reviewing", hasKifuMoves = false } = options;

    const [cellSize, setCellSize] = useState(() => {
        return getInitialCellSize(gameMode, hasKifuMoves);
    });

    const prevSizeRef = useRef(cellSize);
    const gameModeRef = useRef(gameMode);
    const hasKifuMovesRef = useRef(hasKifuMoves);

    // gameModeまたはhasKifuMovesが変わったら再計算
    useEffect(() => {
        if (typeof window === "undefined") return;
        if (window.innerWidth >= MOBILE_BREAKPOINT) return;

        // モードが変わった場合のみ再計算
        if (gameModeRef.current !== gameMode || hasKifuMovesRef.current !== hasKifuMoves) {
            gameModeRef.current = gameMode;
            hasKifuMovesRef.current = hasKifuMoves;

            const newSize = calcOptimalCellSize(
                window.innerWidth,
                window.innerHeight,
                gameMode,
                hasKifuMoves,
            );

            if (newSize !== prevSizeRef.current) {
                prevSizeRef.current = newSize;
                setCellSize(newSize);
            }
        }
    }, [gameMode, hasKifuMoves]);

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

            const newSize = calcOptimalCellSize(
                window.innerWidth,
                window.innerHeight,
                gameModeRef.current,
                hasKifuMovesRef.current,
            );

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
