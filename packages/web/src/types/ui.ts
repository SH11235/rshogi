/**
 * UI State Types
 * 明確なUI状態の型定義
 */

import type { Piece, PieceType, Player, Square } from "shogi-core";

/**
 * プロモーション待機状態
 */
export interface PromotionPendingState {
    from: Square;
    to: Square;
    piece: Piece;
}

/**
 * ドロップ選択状態
 */
export interface SelectedDropPiece {
    type: PieceType;
    player: Player;
}

/**
 * UIゲーム状態
 * ゲームロジックとは別のUI固有の状態
 */
export interface UIGameState {
    /** 選択中のマス */
    selectedSquare: Square | null;
    /** 選択中のドロップ駒 */
    selectedDropPiece: SelectedDropPiece | null;
    /** 有効な移動先 */
    validMoves: Square[];
    /** 有効なドロップ先 */
    validDropSquares: Square[];
    /** プロモーション待機中 */
    promotionPending: PromotionPendingState | null;
}

/**
 * UI操作のコールバック型
 */
export interface UIActions {
    /** マス選択 */
    onSquareSelect: (square: Square) => void;
    /** ドロップ駒選択 */
    onDropPieceSelect: (pieceType: PieceType, player: Player) => void;
    /** プロモーション確認 */
    onPromotionConfirm: (promote: boolean) => void;
    /** プロモーションキャンセル */
    onPromotionCancel: () => void;
}

/**
 * ゲーム操作のコールバック型
 */
export interface GameActions {
    /** ゲームリセット */
    onReset: () => void;
    /** 投了 */
    onResign: () => void;
    /** ゲームインポート */
    onImportGame: (kifContent: string) => void;
}

/**
 * 履歴操作のコールバック型
 */
export interface HistoryActions {
    /** 元に戻す */
    onUndo: () => void;
    /** やり直す */
    onRedo: () => void;
    /** 特定の手に移動 */
    onGoToMove: (moveIndex: number) => void;
}
