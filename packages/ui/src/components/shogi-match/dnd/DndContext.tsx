/**
 * 編集モード DnD コンテキストプロバイダー
 *
 * DnD の状態と操作を子コンポーネントに提供
 */

import type { Piece, PieceType, Player, PositionState, Square } from "@shogi/app-core";
import { createContext, type ReactNode, useCallback, useContext, useMemo } from "react";
import type { DndConfig, DragOrigin, DragPayload, DropResult } from "./types";
import { type DragEnvironment, useDragEnvironment } from "./useDragEnvironment";
import { type PieceDndController, usePieceDnd } from "./usePieceDnd";

/**
 * DnD コンテキストの型
 */
export interface EditDndContextValue {
    /** DnD 環境（ref 群） */
    env: DragEnvironment;
    /** DnD コントローラー */
    controller: PieceDndController;
    /** 編集モードかどうか */
    isEditMode: boolean;
    /** DnD が有効かどうか */
    enabled: boolean;
    /** 駒のドラッグを開始（board から） */
    startBoardDrag: (square: Square, piece: Piece, e: React.PointerEvent) => void;
    /** 駒のドラッグを開始（hand から） */
    startHandDrag: (owner: Player, pieceType: PieceType, e: React.PointerEvent) => void;
    /** 駒のドラッグを開始（stock から） */
    startStockDrag: (
        owner: Player,
        pieceType: PieceType,
        promoted: boolean,
        e: React.PointerEvent,
    ) => void;
}

const EditDndContext = createContext<EditDndContextValue | null>(null);

export interface EditDndProviderProps {
    children: ReactNode;
    /** 編集モードかどうか */
    isEditMode: boolean;
    /** 盤の向き */
    orientation?: "sente" | "gote";
    /** 現在の局面（ドロップ適用時に必要） */
    position: PositionState;
    /** ドロップ時のコールバック */
    onDrop?: (result: DropResult, position: PositionState) => void;
    /** DnD 設定 */
    config?: Partial<DndConfig>;
}

export function EditDndProvider({
    children,
    isEditMode,
    orientation = "sente",
    position,
    onDrop,
    config,
}: EditDndProviderProps): ReactNode {
    const env = useDragEnvironment({ orientation });

    const handleDrop = useCallback(
        (result: DropResult) => {
            onDrop?.(result, position);
        },
        [onDrop, position],
    );

    const controller = usePieceDnd({
        env,
        onDrop: handleDrop,
        config,
        disabled: !isEditMode,
    });

    // ボード上の駒からドラッグ開始
    const startBoardDrag = useCallback(
        (square: Square, piece: Piece, e: React.PointerEvent) => {
            if (!isEditMode) return;

            const origin: DragOrigin = { type: "board", square };
            const payload: DragPayload = {
                owner: piece.owner,
                pieceType: piece.type,
                isPromoted: piece.promoted ?? false,
            };

            controller.startDrag(origin, payload, e);
        },
        [isEditMode, controller],
    );

    // 持ち駒からドラッグ開始
    const startHandDrag = useCallback(
        (owner: Player, pieceType: PieceType, e: React.PointerEvent) => {
            if (!isEditMode) return;

            const origin: DragOrigin = { type: "hand", owner, pieceType };
            const payload: DragPayload = {
                owner,
                pieceType,
                isPromoted: false,
            };

            controller.startDrag(origin, payload, e);
        },
        [isEditMode, controller],
    );

    // ストックからドラッグ開始
    const startStockDrag = useCallback(
        (owner: Player, pieceType: PieceType, promoted: boolean, e: React.PointerEvent) => {
            if (!isEditMode) return;

            const origin: DragOrigin = { type: "stock", owner, pieceType };
            const payload: DragPayload = {
                owner,
                pieceType,
                isPromoted: promoted,
            };

            controller.startDrag(origin, payload, e);
        },
        [isEditMode, controller],
    );

    const value = useMemo<EditDndContextValue>(
        () => ({
            env,
            controller,
            isEditMode,
            enabled: isEditMode,
            startBoardDrag,
            startHandDrag,
            startStockDrag,
        }),
        [env, controller, isEditMode, startBoardDrag, startHandDrag, startStockDrag],
    );

    return <EditDndContext.Provider value={value}>{children}</EditDndContext.Provider>;
}

/**
 * DnD コンテキストを使用
 */
export function useEditDnd(): EditDndContextValue {
    const context = useContext(EditDndContext);
    if (!context) {
        throw new Error("useEditDnd must be used within EditDndProvider");
    }
    return context;
}

/**
 * DnD コンテキストを使用（オプショナル）
 * Provider がない場合は null を返す
 */
export function useEditDndOptional(): EditDndContextValue | null {
    return useContext(EditDndContext);
}
