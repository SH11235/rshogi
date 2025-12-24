/**
 * DnD 環境の ref を管理する hook
 *
 * 盤面・持ち駒・削除ゾーンの ref を集約
 *
 * 注: elementFromPoint 方式に移行したため、計測機能は不要になりました。
 * ref は data 属性を持つ要素に接続するために使用します。
 */

import { useRef } from "react";

export interface DragEnvironment {
    /** 盤面要素の ref */
    boardRef: React.RefObject<HTMLElement | null>;
    /** 先手持ち駒エリアの ref */
    senteHandRef: React.RefObject<HTMLElement | null>;
    /** 後手持ち駒エリアの ref */
    goteHandRef: React.RefObject<HTMLElement | null>;
    /** 削除ゾーンの ref */
    deleteZoneRef: React.RefObject<HTMLElement | null>;
}

interface UseDragEnvironmentOptions {
    /** 盤の向き（後手視点なら 'gote'）- 現在は未使用 */
    orientation?: "sente" | "gote";
}

export function useDragEnvironment(_options: UseDragEnvironmentOptions = {}): DragEnvironment {
    const boardRef = useRef<HTMLElement | null>(null);
    const senteHandRef = useRef<HTMLElement | null>(null);
    const goteHandRef = useRef<HTMLElement | null>(null);
    const deleteZoneRef = useRef<HTMLElement | null>(null);

    return {
        boardRef,
        senteHandRef,
        goteHandRef,
        deleteZoneRef,
    };
}
