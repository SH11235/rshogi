import type { Player } from "@shogi/app-core";
import { useCallback, useEffect, useRef, useState } from "react";

// クロック更新インターバル（ms）
const CLOCK_UPDATE_INTERVAL_MS = 200;

/**
 * 各プレイヤーの時間設定
 */
export type ClockSettings = Record<Player, { mainMs: number; byoyomiMs: number }>;

/**
 * 時計の状態
 */
export interface ClockState {
    mainMs: number;
    byoyomiMs: number;
}

/**
 * 全体の時計状態（両プレイヤー + 現在刻んでいる側）
 */
export interface TickState {
    sente: ClockState;
    gote: ClockState;
    ticking: Player | null;
    lastUpdatedAt: number;
}

/**
 * useClockManager の props
 */
export interface UseClockManagerProps {
    /** 時間設定 */
    timeSettings: ClockSettings;
    /** 対局が実行中かどうか */
    isMatchRunning: boolean;
    /** 時間切れ時に呼ばれるコールバック */
    onTimeExpired: (side: Player) => Promise<void>;
    /** matchEndedRef (時間切れ判定の重複防止用) */
    matchEndedRef: React.MutableRefObject<boolean>;
}

/**
 * useClockManager の返り値
 */
export interface UseClockManagerReturn {
    /** 現在の時計状態 */
    clocks: TickState;
    /** 時計をリセットする */
    resetClocks: (startTick: boolean) => void;
    /** 次の手番に時計を更新する（秒読み時間をリセット） */
    updateClocksForNextTurn: (nextTurn: Player) => void;
    /** 時計を停止する */
    stopTicking: () => void;
    /** 時計を開始する */
    startTicking: (turn: Player) => void;
}

/**
 * TickState を初期化する
 */
export function initialTick(settings: ClockSettings): TickState {
    return {
        sente: { mainMs: settings.sente.mainMs, byoyomiMs: settings.sente.byoyomiMs },
        gote: { mainMs: settings.gote.mainMs, byoyomiMs: settings.gote.byoyomiMs },
        ticking: null,
        lastUpdatedAt: Date.now(),
    };
}

/**
 * 時計管理のカスタムフック
 *
 * ゲーム進行中の時間管理を行います。
 * - 持ち時間と秒読み時間の管理
 * - 定期的な時計の更新（200ms間隔）
 * - 時間切れの判定と通知
 *
 * @param props - フックの設定
 * @returns 時計状態と操作関数
 *
 * @example
 * ```typescript
 * const { clocks, resetClocks, updateClocksForNextTurn } = useClockManager({
 *   timeSettings: { sente: { mainMs: 600000, byoyomiMs: 10000 }, gote: { ... } },
 *   isMatchRunning: true,
 *   onTimeExpired: async (side) => {
 *     console.log(`${side} の時間切れ`);
 *   },
 *   matchEndedRef,
 * });
 * ```
 */
export function useClockManager({
    timeSettings,
    isMatchRunning,
    onTimeExpired,
    matchEndedRef,
}: UseClockManagerProps): UseClockManagerReturn {
    const [clocks, setClocks] = useState<TickState>(() => initialTick(timeSettings));

    // onTimeExpired を ref に保存（依存配列の安定化）
    const onTimeExpiredRef = useRef(onTimeExpired);
    useEffect(() => {
        onTimeExpiredRef.current = onTimeExpired;
    }, [onTimeExpired]);

    /**
     * 時計をリセットする
     */
    const resetClocks = useCallback(
        (startTick: boolean) => {
            setClocks({
                sente: {
                    mainMs: timeSettings.sente.mainMs,
                    byoyomiMs: timeSettings.sente.byoyomiMs,
                },
                gote: {
                    mainMs: timeSettings.gote.mainMs,
                    byoyomiMs: timeSettings.gote.byoyomiMs,
                },
                ticking: startTick ? "sente" : null,
                lastUpdatedAt: Date.now(),
            });
        },
        [timeSettings],
    );

    /**
     * 次の手番に時計を更新する（秒読み時間をリセット）
     */
    const updateClocksForNextTurn = useCallback(
        (nextTurn: Player) => {
            setClocks((prev) => ({
                ...prev,
                [nextTurn]: {
                    mainMs: prev[nextTurn].mainMs,
                    byoyomiMs: timeSettings[nextTurn].byoyomiMs,
                },
                ticking: nextTurn,
                lastUpdatedAt: Date.now(),
            }));
        },
        [timeSettings],
    );

    /**
     * 時計を停止する
     */
    const stopTicking = useCallback(() => {
        setClocks((prev) => ({ ...prev, ticking: null }));
    }, []);

    /**
     * 時計を開始する
     */
    const startTicking = useCallback((turn: Player) => {
        setClocks((prev) => ({ ...prev, ticking: turn, lastUpdatedAt: Date.now() }));
    }, []);

    // 時計の定期更新（200ms間隔）
    useEffect(() => {
        if (!isMatchRunning || !clocks.ticking) return;

        const timer = setInterval(() => {
            let expiredSide: Player | null = null;

            setClocks((prev) => {
                if (!prev.ticking) return prev;

                const now = Date.now();
                const delta = now - prev.lastUpdatedAt;
                const side = prev.ticking;
                const current = prev[side];

                let mainMs = current.mainMs - delta;
                let byoyomiMs = current.byoyomiMs;

                // 持ち時間が負になった分を秒読み時間から減少
                if (mainMs < 0) {
                    const over = Math.abs(mainMs);
                    mainMs = 0;
                    byoyomiMs = Math.max(0, byoyomiMs - over);
                }

                // 両方の時間が尽きたかチェック
                if (mainMs <= 0 && byoyomiMs <= 0) {
                    expiredSide = side;
                }

                return {
                    ...prev,
                    [side]: { mainMs: Math.max(0, mainMs), byoyomiMs },
                    lastUpdatedAt: now,
                };
            });

            // 時間切れの処理（setClocks の外で実行）
            if (expiredSide && isMatchRunning && !matchEndedRef.current) {
                onTimeExpiredRef
                    .current(expiredSide)
                    .catch((err) => console.error("時間切れ処理エラー:", err));
            }
        }, CLOCK_UPDATE_INTERVAL_MS);

        return () => clearInterval(timer);
    }, [clocks.ticking, isMatchRunning, matchEndedRef]);

    return {
        clocks,
        resetClocks,
        updateClocksForNextTurn,
        stopTicking,
        startTicking,
    };
}
