/**
 * 将棋盤 編集モード DnD 型定義
 *
 * 設計書: docs/edit-mode-dnd-design-refined.md
 */

import type { PieceType, Player, Square } from "@shogi/app-core";

/**
 * ドラッグ元の実体
 * - board: 盤上の駒
 * - hand: 持ち駒
 * - stock: ストック（無限供給の駒）
 */
export type DragOrigin =
    | { type: "board"; square: Square }
    | { type: "hand"; owner: Player; pieceType: PieceType }
    | { type: "stock"; owner: Player; pieceType: PieceType };

/**
 * ドラッグ中の駒情報（表示用）
 */
export interface DragPayload {
    owner: Player;
    pieceType: PieceType;
    isPromoted: boolean;
}

/**
 * ドロップ先
 * - board: 盤上のマス
 * - hand: 持ち駒エリア
 * - delete: 有効エリア外/削除ゾーン
 */
export type DropTarget =
    | { type: "board"; square: Square }
    | { type: "hand"; owner: Player }
    | { type: "delete" };

/**
 * DnD ランタイム状態（ref で管理、React state ではない）
 */
export interface DragRuntime {
    active: boolean;
    pointerId: number | null;
    pointerType: "mouse" | "touch" | "pen" | null;
    captureTarget: Element | null;
    startClient: { x: number; y: number };
    lastClient: { x: number; y: number };
    longPressTimer: ReturnType<typeof setTimeout> | null;
    raf: number | null;

    origin: DragOrigin | null;
    payload: DragPayload | null;
    hover: DropTarget | null;
}

/**
 * DnD コンテキストで公開する React state
 */
export interface DndState {
    isDragging: boolean;
    payload: DragPayload | null;
    hoverTarget: DropTarget | null;
    mode: "valid" | "invalid" | "delete" | null;
}

/**
 * ドロップ結果
 */
export interface DropResult {
    origin: DragOrigin;
    payload: DragPayload;
    target: DropTarget;
}

/**
 * DnD 設定
 */
export interface DndConfig {
    /** ロングプレス開始までの時間(ms) */
    longPressMs: number;
    /** スロップ（移動量しきい値）(px) */
    slopPx: number;
    /** エリア外をキャンセルにするか削除にするか */
    outsideAreaBehavior: "delete" | "cancel";
}

export const DEFAULT_DND_CONFIG: DndConfig = {
    longPressMs: 280,
    slopPx: 10,
    outsideAreaBehavior: "delete",
};
