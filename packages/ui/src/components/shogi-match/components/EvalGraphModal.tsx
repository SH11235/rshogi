/**
 * 評価値グラフ拡大表示ウィンドウ（ドラッグ・リサイズ可能）
 *
 * 非モーダル：背景操作をブロックしない
 * 四隅＋四辺からリサイズ可能
 */

import type { ReactElement } from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { EvalHistory } from "../utils/kifFormat";
import { EvalGraph } from "./EvalGraph";

interface EvalGraphModalProps {
    /** 評価値の履歴 */
    evalHistory: EvalHistory[];
    /** 現在の手数 */
    currentPly: number;
    /** ウィンドウの開閉状態 */
    open: boolean;
    /** 閉じる時のコールバック */
    onClose: () => void;
}

interface Position {
    x: number;
    y: number;
}

interface Size {
    width: number;
    height: number;
}

/** ドラッグモード: none=なし, move=移動, resize-XX=リサイズ（隅・辺） */
type DragMode =
    | "none"
    | "move"
    | "resize-n"
    | "resize-s"
    | "resize-e"
    | "resize-w"
    | "resize-ne"
    | "resize-nw"
    | "resize-se"
    | "resize-sw";

const MIN_WIDTH = 300;
const MIN_HEIGHT = 200;
const HEADER_HEIGHT = 40;
const CONTENT_PADDING = 20;
const X_AXIS_LABEL_HEIGHT = 20;
const EDGE_HANDLE_SIZE = 6;

/**
 * 評価値グラフを拡大表示するドラッグ可能ウィンドウ
 */
export function EvalGraphModal({
    evalHistory,
    currentPly,
    open,
    onClose,
}: EvalGraphModalProps): ReactElement | null {
    const [position, setPosition] = useState<Position>(() => ({
        x: typeof window !== "undefined" ? window.innerWidth / 2 - 300 : 100,
        y: typeof window !== "undefined" ? window.innerHeight / 2 - 200 : 100,
    }));
    const [size, setSize] = useState<Size>({ width: 600, height: 400 });

    const dragMode = useRef<DragMode>("none");
    const dragStart = useRef<Position>({ x: 0, y: 0 });
    const initialPosition = useRef<Position>({ x: 0, y: 0 });
    const initialSize = useRef<Size>({ width: 0, height: 0 });

    // ドラッグ開始（移動）
    const handleMoveStart = useCallback(
        (e: React.MouseEvent) => {
            e.preventDefault();
            dragMode.current = "move";
            dragStart.current = { x: e.clientX, y: e.clientY };
            initialPosition.current = { ...position };
        },
        [position],
    );

    // リサイズ開始（共通）
    const createResizeHandler = useCallback(
        (mode: DragMode) => (e: React.MouseEvent) => {
            e.preventDefault();
            e.stopPropagation();
            dragMode.current = mode;
            dragStart.current = { x: e.clientX, y: e.clientY };
            initialPosition.current = { ...position };
            initialSize.current = { ...size };
        },
        [position, size],
    );

    // Escキーでウィンドウを閉じる
    useEffect(() => {
        if (!open) return;

        const handleEscape = (e: KeyboardEvent) => {
            if (e.key === "Escape") {
                onClose();
            }
        };

        document.addEventListener("keydown", handleEscape);
        return () => document.removeEventListener("keydown", handleEscape);
    }, [open, onClose]);

    // マウス移動・終了のグローバルハンドラ
    useEffect(() => {
        const handleMouseMove = (e: MouseEvent) => {
            if (dragMode.current === "none") return;

            const deltaX = e.clientX - dragStart.current.x;
            const deltaY = e.clientY - dragStart.current.y;

            if (dragMode.current === "move") {
                const newX = initialPosition.current.x + deltaX;
                const newY = initialPosition.current.y + deltaY;
                const maxX = window.innerWidth - size.width;
                const maxY = window.innerHeight - size.height;
                setPosition({
                    x: Math.max(0, Math.min(newX, maxX)),
                    y: Math.max(0, Math.min(newY, maxY)),
                });
            } else if (dragMode.current === "resize-e") {
                // 右辺: 幅のみ増減
                const newWidth = initialSize.current.width + deltaX;
                const maxWidth = window.innerWidth - position.x;
                setSize((prev) => ({
                    ...prev,
                    width: Math.max(MIN_WIDTH, Math.min(newWidth, maxWidth)),
                }));
            } else if (dragMode.current === "resize-w") {
                // 左辺: 幅を左方向に増減
                const newX = initialPosition.current.x + deltaX;
                const maxX = initialPosition.current.x + initialSize.current.width - MIN_WIDTH;
                const clampedX = Math.max(0, Math.min(newX, maxX));
                const clampedWidth =
                    initialPosition.current.x + initialSize.current.width - clampedX;

                setSize((prev) => ({ ...prev, width: Math.max(MIN_WIDTH, clampedWidth) }));
                setPosition((prev) => ({ ...prev, x: clampedX }));
            } else if (dragMode.current === "resize-s") {
                // 下辺: 高さのみ増減
                const newHeight = initialSize.current.height + deltaY;
                const maxHeight = window.innerHeight - position.y;
                setSize((prev) => ({
                    ...prev,
                    height: Math.max(MIN_HEIGHT, Math.min(newHeight, maxHeight)),
                }));
            } else if (dragMode.current === "resize-n") {
                // 上辺: 高さを上方向に増減
                const newY = initialPosition.current.y + deltaY;
                const maxY = initialPosition.current.y + initialSize.current.height - MIN_HEIGHT;
                const clampedY = Math.max(0, Math.min(newY, maxY));
                const clampedHeight =
                    initialPosition.current.y + initialSize.current.height - clampedY;

                setSize((prev) => ({ ...prev, height: Math.max(MIN_HEIGHT, clampedHeight) }));
                setPosition((prev) => ({ ...prev, y: clampedY }));
            } else if (dragMode.current === "resize-se") {
                // 右下: 幅と高さを増減
                const newWidth = initialSize.current.width + deltaX;
                const newHeight = initialSize.current.height + deltaY;
                const maxWidth = window.innerWidth - position.x;
                const maxHeight = window.innerHeight - position.y;
                setSize({
                    width: Math.max(MIN_WIDTH, Math.min(newWidth, maxWidth)),
                    height: Math.max(MIN_HEIGHT, Math.min(newHeight, maxHeight)),
                });
            } else if (dragMode.current === "resize-ne") {
                // 右上: 幅を増減、高さは上方向に増減
                const newWidth = initialSize.current.width + deltaX;
                const newY = initialPosition.current.y + deltaY;
                const maxWidth = window.innerWidth - position.x;
                const clampedWidth = Math.max(MIN_WIDTH, Math.min(newWidth, maxWidth));
                const maxY = initialPosition.current.y + initialSize.current.height - MIN_HEIGHT;
                const clampedY = Math.max(0, Math.min(newY, maxY));
                const clampedHeight =
                    initialPosition.current.y + initialSize.current.height - clampedY;

                setSize({ width: clampedWidth, height: Math.max(MIN_HEIGHT, clampedHeight) });
                setPosition((prev) => ({ ...prev, y: clampedY }));
            } else if (dragMode.current === "resize-sw") {
                // 左下: 幅は左方向に増減、高さを増減
                const newX = initialPosition.current.x + deltaX;
                const newHeight = initialSize.current.height + deltaY;

                const maxX = initialPosition.current.x + initialSize.current.width - MIN_WIDTH;
                const clampedX = Math.max(0, Math.min(newX, maxX));
                const clampedWidth =
                    initialPosition.current.x + initialSize.current.width - clampedX;
                const maxHeight = window.innerHeight - position.y;

                setSize({
                    width: Math.max(MIN_WIDTH, clampedWidth),
                    height: Math.max(MIN_HEIGHT, Math.min(newHeight, maxHeight)),
                });
                setPosition((prev) => ({ ...prev, x: clampedX }));
            } else if (dragMode.current === "resize-nw") {
                // 左上: 幅と高さを左上方向に増減
                const newX = initialPosition.current.x + deltaX;
                const newY = initialPosition.current.y + deltaY;

                const maxX = initialPosition.current.x + initialSize.current.width - MIN_WIDTH;
                const clampedX = Math.max(0, Math.min(newX, maxX));
                const clampedWidth =
                    initialPosition.current.x + initialSize.current.width - clampedX;

                const maxY = initialPosition.current.y + initialSize.current.height - MIN_HEIGHT;
                const clampedY = Math.max(0, Math.min(newY, maxY));
                const clampedHeight =
                    initialPosition.current.y + initialSize.current.height - clampedY;

                setSize({
                    width: Math.max(MIN_WIDTH, clampedWidth),
                    height: Math.max(MIN_HEIGHT, clampedHeight),
                });
                setPosition({ x: clampedX, y: clampedY });
            }
        };

        const handleMouseUp = () => {
            dragMode.current = "none";
        };

        if (open) {
            document.addEventListener("mousemove", handleMouseMove);
            document.addEventListener("mouseup", handleMouseUp);
        }

        return () => {
            document.removeEventListener("mousemove", handleMouseMove);
            document.removeEventListener("mouseup", handleMouseUp);
        };
    }, [open, size.width, size.height, position.x, position.y]);

    if (!open) {
        return null;
    }

    const graphHeight = Math.max(
        size.height - HEADER_HEIGHT - CONTENT_PADDING - X_AXIS_LABEL_HEIGHT,
        100,
    );

    return (
        <div
            className="fixed flex flex-col overflow-hidden bg-card border border-border rounded-xl shadow-2xl z-[1000]"
            style={{
                left: position.x,
                top: position.y,
                width: size.width,
                height: size.height,
            }}
        >
            {/* ヘッダー（ドラッグハンドル） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: drag handle for window movement */}
            <div
                className="flex justify-between items-center px-3 py-2 bg-muted border-b border-border cursor-move select-none h-10 box-border"
                onMouseDown={handleMoveStart}
                role="presentation"
            >
                <span className="font-semibold text-sm">評価値推移</span>
                <button
                    type="button"
                    className="bg-transparent border-none cursor-pointer px-2 py-1 rounded text-base leading-none text-muted-foreground hover:bg-accent"
                    onClick={onClose}
                    aria-label="閉じる"
                >
                    ✕
                </button>
            </div>

            {/* グラフ本体 */}
            <div className="flex-1 p-3 pb-2 overflow-visible">
                <EvalGraph
                    evalHistory={evalHistory}
                    currentPly={currentPly}
                    height={graphHeight}
                    compact
                />
            </div>

            {/* 辺リサイズハンドル（上） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle */}
            <div
                className="absolute top-0 left-3 right-3 cursor-ns-resize"
                style={{ height: EDGE_HANDLE_SIZE }}
                onMouseDown={createResizeHandler("resize-n")}
                role="presentation"
            />

            {/* 辺リサイズハンドル（下） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle */}
            <div
                className="absolute bottom-0 left-3 right-3 cursor-ns-resize"
                style={{ height: EDGE_HANDLE_SIZE }}
                onMouseDown={createResizeHandler("resize-s")}
                role="presentation"
            />

            {/* 辺リサイズハンドル（左） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle */}
            <div
                className="absolute left-0 top-3 bottom-3 cursor-ew-resize"
                style={{ width: EDGE_HANDLE_SIZE }}
                onMouseDown={createResizeHandler("resize-w")}
                role="presentation"
            />

            {/* 辺リサイズハンドル（右） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle */}
            <div
                className="absolute right-0 top-3 bottom-3 cursor-ew-resize"
                style={{ width: EDGE_HANDLE_SIZE }}
                onMouseDown={createResizeHandler("resize-e")}
                role="presentation"
            />

            {/* 隅リサイズハンドル（左上） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle */}
            <div
                className="absolute left-0 top-0 w-3 h-3 cursor-nwse-resize"
                onMouseDown={createResizeHandler("resize-nw")}
                role="presentation"
            >
                <div className="absolute left-1 top-1 w-2 h-2 border-l-2 border-t-2 border-muted-foreground opacity-50" />
            </div>

            {/* 隅リサイズハンドル（右上） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle */}
            <div
                className="absolute right-0 top-0 w-3 h-3 cursor-nesw-resize"
                onMouseDown={createResizeHandler("resize-ne")}
                role="presentation"
            >
                <div className="absolute right-1 top-1 w-2 h-2 border-r-2 border-t-2 border-muted-foreground opacity-50" />
            </div>

            {/* 隅リサイズハンドル（左下） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle */}
            <div
                className="absolute left-0 bottom-0 w-3 h-3 cursor-nesw-resize"
                onMouseDown={createResizeHandler("resize-sw")}
                role="presentation"
            >
                <div className="absolute left-1 bottom-1 w-2 h-2 border-l-2 border-b-2 border-muted-foreground opacity-50" />
            </div>

            {/* 隅リサイズハンドル（右下） */}
            {/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle */}
            <div
                className="absolute right-0 bottom-0 w-3 h-3 cursor-nwse-resize"
                onMouseDown={createResizeHandler("resize-se")}
                role="presentation"
            >
                <div className="absolute right-1 bottom-1 w-2 h-2 border-r-2 border-b-2 border-muted-foreground opacity-50" />
            </div>
        </div>
    );
}
