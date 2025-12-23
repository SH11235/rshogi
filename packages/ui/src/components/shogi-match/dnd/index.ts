/**
 * 将棋盤 編集モード DnD モジュール
 *
 * ref + rAF 方式でパフォーマンス最適化された DnD 実装
 * 設計書: docs/edit-mode-dnd-design-refined.md
 */

// コンポーネント
export type { DeleteZoneProps } from "./DeleteZone";
export { DeleteZone } from "./DeleteZone";
// コンテキスト
export type { EditDndContextValue, EditDndProviderProps } from "./DndContext";
export { EditDndProvider, useEditDnd, useEditDndOptional } from "./DndContext";
export type { DragGhostProps } from "./DragGhost";
export { DragGhost } from "./DragGhost";
// ドロップロジック
export type { ApplyDropResult, ValidateDropResult } from "./dropLogic";
export { applyDrop, applyDropResult, validateDrop } from "./dropLogic";
// ヒットテスト
export {
    dropTargetEquals,
    getDropTarget,
    hitTestBoard,
    hitTestZones,
    measureBoard,
    measureZones,
} from "./hit-test";
// 型定義
export type {
    BoardMetrics,
    DndConfig,
    DndState,
    DragOrigin,
    DragPayload,
    DragRuntime,
    DragStartEvent,
    DropResult,
    DropTarget,
    Zones,
} from "./types";
export { DEFAULT_DND_CONFIG } from "./types";
// Hooks
export type { DragEnvironment, UseDragEnvironmentOptions } from "./useDragEnvironment";
export { useDragEnvironment } from "./useDragEnvironment";
export type { PieceDndController, UsePieceDndOptions } from "./usePieceDnd";
export { usePieceDnd } from "./usePieceDnd";
