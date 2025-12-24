/**
 * DnD 環境の ref と計測を管理する hook
 *
 * 盤面・持ち駒・削除ゾーンの ref を集約し、
 * ドラッグ開始時や resize 時に rect を再計測する
 */

import { useCallback, useRef } from "react";
import { measureBoard, measureZones } from "./hitDetection";
import type { BoardMetrics, Zones } from "./types";

export interface DragEnvironment {
    /** 盤面要素の ref */
    boardRef: React.RefObject<HTMLElement | null>;
    /** 先手持ち駒エリアの ref */
    senteHandRef: React.RefObject<HTMLElement | null>;
    /** 後手持ち駒エリアの ref */
    goteHandRef: React.RefObject<HTMLElement | null>;
    /** 削除ゾーンの ref */
    deleteZoneRef: React.RefObject<HTMLElement | null>;
    /** 盤の向き */
    orientation: "sente" | "gote";
    /** 計測結果をキャッシュ */
    metricsCache: React.MutableRefObject<{
        board: BoardMetrics | null;
        zones: Zones | null;
    }>;
    /** 計測を実行 */
    measure: () => { board: BoardMetrics | null; zones: Zones };
}

interface UseDragEnvironmentOptions {
    /** 盤の向き（後手視点なら 'gote'） */
    orientation?: "sente" | "gote";
}

export function useDragEnvironment(options: UseDragEnvironmentOptions = {}): DragEnvironment {
    const { orientation = "sente" } = options;

    const boardRef = useRef<HTMLElement | null>(null);
    const senteHandRef = useRef<HTMLElement | null>(null);
    const goteHandRef = useRef<HTMLElement | null>(null);
    const deleteZoneRef = useRef<HTMLElement | null>(null);

    const metricsCache = useRef<{
        board: BoardMetrics | null;
        zones: Zones | null;
    }>({
        board: null,
        zones: null,
    });

    const measure = useCallback(() => {
        const board = boardRef.current ? measureBoard(boardRef.current, orientation) : null;
        const zones = measureZones(
            senteHandRef.current,
            goteHandRef.current,
            deleteZoneRef.current,
        );

        metricsCache.current = { board, zones };
        return { board, zones };
    }, [orientation]);

    return {
        boardRef,
        senteHandRef,
        goteHandRef,
        deleteZoneRef,
        orientation,
        metricsCache,
        measure,
    };
}
