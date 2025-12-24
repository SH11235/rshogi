/**
 * ドロップ適用ロジック（純関数コア）
 *
 * DnD のドロップ結果を局面に適用する
 */

import type { Piece, PositionState } from "@shogi/app-core";
import { cloneBoard } from "@shogi/app-core";
import { addToHand, cloneHandsState, consumeFromHand, countPieces } from "../utils/boardUtils";
import { PIECE_CAP } from "../utils/constants";
import type { DragOrigin, DragPayload, DropResult, DropTarget } from "./types";

/**
 * ドロップ検証結果
 */
interface ValidateDropResult {
    ok: boolean;
    error?: string;
    /** 成りを強制解除する場合（行き場のない駒など） */
    forceUnpromote?: boolean;
}

/**
 * ドロップ適用結果
 */
interface ApplyDropResult {
    ok: boolean;
    next: PositionState;
    error?: string;
}

/**
 * ドロップを検証
 */
export function validateDrop(
    origin: DragOrigin,
    payload: DragPayload,
    target: DropTarget,
    position: PositionState,
): ValidateDropResult {
    // 削除は常に許可
    if (target.type === "delete") {
        return { ok: true };
    }

    // 持ち駒エリアへのドロップ
    if (target.type === "hand") {
        // board からのみ許可（hand/stock → hand は意味がない）
        if (origin.type !== "board") {
            return { ok: false, error: "持ち駒から持ち駒への移動はできません" };
        }
        // 成り駒は生駒に戻る（自動）
        return { ok: true };
    }

    // 盤上へのドロップ
    if (target.type === "board") {
        const sq = target.square;
        const existing = position.board[sq];

        // 自分の駒がある場所には置けない（交換はしない）
        // ただし、同じ駒を同じ場所に戻すのは OK
        if (origin.type === "board" && origin.square === sq) {
            return { ok: true }; // 同じ場所 = キャンセル扱い
        }

        if (existing && existing.owner === payload.owner) {
            return { ok: false, error: "自分の駒がある場所には置けません" };
        }

        // 駒数制限チェック
        const baseType = payload.pieceType;
        const counts = countPieces(position);
        const currentCount = counts[payload.owner][baseType];

        // origin が board の場合は移動なのでカウントは変わらない
        // origin が hand/stock の場合は追加
        if (origin.type !== "board") {
            if (currentCount >= PIECE_CAP[baseType]) {
                return {
                    ok: false,
                    error: `${baseType}は最大${PIECE_CAP[baseType]}枚までです`,
                };
            }
        }

        // 玉は1枚まで
        if (baseType === "K" && origin.type !== "board") {
            if (currentCount >= 1) {
                return { ok: false, error: "玉は1枚までです" };
            }
        }

        return { ok: true };
    }

    return { ok: false, error: "不明なドロップ先" };
}

/**
 * ドロップを適用
 */
export function applyDrop(
    origin: DragOrigin,
    payload: DragPayload,
    target: DropTarget,
    position: PositionState,
): ApplyDropResult {
    const validation = validateDrop(origin, payload, target, position);
    if (!validation.ok) {
        return { ok: false, next: position, error: validation.error };
    }

    const nextBoard = cloneBoard(position.board);
    let nextHands = cloneHandsState(position.hands);

    // 元の場所から駒を除去
    if (origin.type === "board") {
        nextBoard[origin.square] = null;
    } else if (origin.type === "hand") {
        const consumed = consumeFromHand(nextHands, origin.owner, origin.pieceType);
        if (consumed) {
            nextHands = consumed;
        }
        // stock の場合は消費しない（無限供給）
    }

    // ターゲットに応じた処理
    if (target.type === "delete") {
        // 削除: origin に応じた処理
        // - board: 単に消す（既に上で null にした）
        // - hand: 単に消す（既に上で消費した）
        // - stock: 何もしない（キャンセル扱い、でも上で消費してないので OK）
        return {
            ok: true,
            next: { ...position, board: nextBoard, hands: nextHands },
        };
    }

    if (target.type === "hand") {
        // 持ち駒に追加（成りは解除）
        const baseType = payload.pieceType;
        nextHands = addToHand(nextHands, target.owner, baseType);
        return {
            ok: true,
            next: { ...position, board: nextBoard, hands: nextHands },
        };
    }

    if (target.type === "board") {
        const sq = target.square;

        // 同じ場所への移動はキャンセル
        if (origin.type === "board" && origin.square === sq) {
            // 元に戻す
            const piece: Piece = {
                owner: payload.owner,
                type: payload.pieceType,
                promoted: payload.isPromoted || undefined,
            };
            nextBoard[sq] = piece;
            return {
                ok: true,
                next: { ...position, board: nextBoard, hands: nextHands },
            };
        }

        // 既存の駒があれば持ち駒に（相手の駒を取る）
        const existing = nextBoard[sq];
        if (existing) {
            // 成り駒は生駒に戻して持ち駒に
            nextHands = addToHand(nextHands, payload.owner, existing.type);
        }

        // 駒を配置
        const piece: Piece = {
            owner: payload.owner,
            type: payload.pieceType,
            promoted: payload.isPromoted || undefined,
        };
        nextBoard[sq] = piece;

        return {
            ok: true,
            next: { ...position, board: nextBoard, hands: nextHands },
        };
    }

    return { ok: false, next: position, error: "不明なドロップ先" };
}

/**
 * DropResult から適用
 */
export function applyDropResult(result: DropResult, position: PositionState): ApplyDropResult {
    return applyDrop(result.origin, result.payload, result.target, position);
}
