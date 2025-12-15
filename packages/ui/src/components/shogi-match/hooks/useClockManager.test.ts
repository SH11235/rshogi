import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type ClockSettings, initialTick, useClockManager } from "./useClockManager";

describe("initialTick", () => {
    it("ClockSettings から TickState を初期化する", () => {
        const settings: ClockSettings = {
            sente: { mainMs: 600000, byoyomiMs: 10000 },
            gote: { mainMs: 300000, byoyomiMs: 5000 },
        };

        const result = initialTick(settings);

        expect(result.sente).toEqual({ mainMs: 600000, byoyomiMs: 10000 });
        expect(result.gote).toEqual({ mainMs: 300000, byoyomiMs: 5000 });
        expect(result.ticking).toBeNull();
        expect(result.lastUpdatedAt).toBeGreaterThan(0);
    });

    it("ticking は null で初期化される", () => {
        const settings: ClockSettings = {
            sente: { mainMs: 0, byoyomiMs: 0 },
            gote: { mainMs: 0, byoyomiMs: 0 },
        };

        const result = initialTick(settings);

        expect(result.ticking).toBeNull();
    });

    it("lastUpdatedAt は現在時刻に近い値で初期化される", () => {
        const before = Date.now();
        const settings: ClockSettings = {
            sente: { mainMs: 100, byoyomiMs: 200 },
            gote: { mainMs: 300, byoyomiMs: 400 },
        };

        const result = initialTick(settings);
        const after = Date.now();

        expect(result.lastUpdatedAt).toBeGreaterThanOrEqual(before);
        expect(result.lastUpdatedAt).toBeLessThanOrEqual(after);
    });
});

describe("useClockManager", () => {
    let matchEndedRef: React.MutableRefObject<boolean>;
    let onTimeExpired: ReturnType<typeof vi.fn>;

    beforeEach(() => {
        matchEndedRef = { current: false };
        onTimeExpired = vi.fn();
        vi.useFakeTimers();
    });

    afterEach(() => {
        vi.restoreAllMocks();
        vi.useRealTimers();
    });

    describe("clock operations", () => {
        it("resetClocks が時間をリセットする（startTick = false）", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 600000, byoyomiMs: 10000 },
                gote: { mainMs: 300000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: false,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.resetClocks(false);
            });

            expect(result.current.clocks.sente.mainMs).toBe(600000);
            expect(result.current.clocks.sente.byoyomiMs).toBe(10000);
            expect(result.current.clocks.gote.mainMs).toBe(300000);
            expect(result.current.clocks.gote.byoyomiMs).toBe(5000);
            expect(result.current.clocks.ticking).toBeNull();
        });

        it("resetClocks が時間をリセットする（startTick = true）", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 100000, byoyomiMs: 5000 },
                gote: { mainMs: 200000, byoyomiMs: 8000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: false,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.resetClocks(true);
            });

            expect(result.current.clocks.ticking).toBe("sente");
        });

        it("updateClocksForNextTurn が秒読み時間をリセットし、手番を切り替える", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 600000, byoyomiMs: 10000 },
                gote: { mainMs: 300000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: false,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            // 先手の時間を減らす
            act(() => {
                result.current.startTicking("sente");
            });

            act(() => {
                vi.advanceTimersByTime(1000);
            });

            // 後手に切り替え
            act(() => {
                result.current.updateClocksForNextTurn("gote");
            });

            expect(result.current.clocks.ticking).toBe("gote");
            expect(result.current.clocks.gote.byoyomiMs).toBe(5000); // リセットされる
        });

        it("stopTicking が時計を停止する", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 100000, byoyomiMs: 5000 },
                gote: { mainMs: 100000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: true,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.startTicking("sente");
            });

            expect(result.current.clocks.ticking).toBe("sente");

            act(() => {
                result.current.stopTicking();
            });

            expect(result.current.clocks.ticking).toBeNull();
        });

        it("startTicking が時計を開始する", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 100000, byoyomiMs: 5000 },
                gote: { mainMs: 100000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: true,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            expect(result.current.clocks.ticking).toBeNull();

            act(() => {
                result.current.startTicking("gote");
            });

            expect(result.current.clocks.ticking).toBe("gote");
        });
    });

    describe("clock time calculation", () => {
        it("持ち時間が正常に減少する", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 10000, byoyomiMs: 5000 },
                gote: { mainMs: 10000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: true,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.startTicking("sente");
            });

            const initialMainMs = result.current.clocks.sente.mainMs;

            act(() => {
                vi.advanceTimersByTime(1000); // 1秒進める
            });

            // 持ち時間が減っているはず
            expect(result.current.clocks.sente.mainMs).toBeLessThan(initialMainMs);
            expect(result.current.clocks.sente.mainMs).toBeGreaterThan(8000); // 約1秒減っている

            // 秒読み時間は減らない
            expect(result.current.clocks.sente.byoyomiMs).toBe(5000);
        });

        it("持ち時間が0になった後、秒読み時間が減少する", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 500, byoyomiMs: 5000 },
                gote: { mainMs: 10000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: true,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.startTicking("sente");
            });

            // 1秒進めて、持ち時間を0にする
            act(() => {
                vi.advanceTimersByTime(1000);
            });

            // 持ち時間が0になり、秒読み時間が減っている
            expect(result.current.clocks.sente.mainMs).toBe(0);
            expect(result.current.clocks.sente.byoyomiMs).toBeLessThan(5000);
            expect(result.current.clocks.sente.byoyomiMs).toBeGreaterThan(3500); // 約500ms残っていた分も考慮
        });

        it("両方が0になったときに時間が0になる", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 100, byoyomiMs: 100 },
                gote: { mainMs: 10000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: true,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.startTicking("sente");
            });

            // 十分な時間を進めて時間切れにする
            act(() => {
                vi.advanceTimersByTime(1000); // 1秒進める
            });

            // 両方の時間が0になっている
            expect(result.current.clocks.sente.mainMs).toBe(0);
            expect(result.current.clocks.sente.byoyomiMs).toBe(0);
        });

        it("時間がマイナスにならない", () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 100, byoyomiMs: 100 },
                gote: { mainMs: 10000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: true,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.startTicking("sente");
            });

            // 十分な時間を進める
            act(() => {
                vi.advanceTimersByTime(2000);
            });

            // 時間がマイナスにならない
            expect(result.current.clocks.sente.mainMs).toBeGreaterThanOrEqual(0);
            expect(result.current.clocks.sente.byoyomiMs).toBeGreaterThanOrEqual(0);
        });

        it("isMatchRunning が false の場合は時間が減少しない", async () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 10000, byoyomiMs: 5000 },
                gote: { mainMs: 10000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: false,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.startTicking("sente");
            });

            const initialMainMs = result.current.clocks.sente.mainMs;

            act(() => {
                vi.advanceTimersByTime(1000);
            });

            // isMatchRunning が false なので時間は減らない
            expect(result.current.clocks.sente.mainMs).toBe(initialMainMs);
        });

        it("ticking が null の場合は時間が減少しない", async () => {
            const timeSettings: ClockSettings = {
                sente: { mainMs: 10000, byoyomiMs: 5000 },
                gote: { mainMs: 10000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: true,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            const initialMainMs = result.current.clocks.sente.mainMs;

            act(() => {
                vi.advanceTimersByTime(1000);
            });

            // ticking が null なので時間は減らない
            expect(result.current.clocks.sente.mainMs).toBe(initialMainMs);
        });

        it("matchEndedRef.current が true の場合は時間切れコールバックが呼ばれない", () => {
            matchEndedRef.current = true;

            const timeSettings: ClockSettings = {
                sente: { mainMs: 100, byoyomiMs: 100 },
                gote: { mainMs: 10000, byoyomiMs: 5000 },
            };

            const { result } = renderHook(() =>
                useClockManager({
                    timeSettings,
                    isMatchRunning: true,
                    onTimeExpired,
                    matchEndedRef,
                }),
            );

            act(() => {
                result.current.startTicking("sente");
            });

            act(() => {
                vi.advanceTimersByTime(500);
            });

            // 時間は0になっている
            expect(result.current.clocks.sente.mainMs).toBe(0);
            expect(result.current.clocks.sente.byoyomiMs).toBe(0);

            // matchEndedRef.current が true なので呼ばれない
            expect(onTimeExpired).not.toHaveBeenCalled();
        });
    });

    describe("time settings update", () => {
        it("timeSettings が変更されたら resetClocks で新しい設定が反映される", () => {
            const initialSettings: ClockSettings = {
                sente: { mainMs: 100000, byoyomiMs: 5000 },
                gote: { mainMs: 100000, byoyomiMs: 5000 },
            };

            const { result, rerender } = renderHook(
                ({ settings }: { settings: ClockSettings }) =>
                    useClockManager({
                        timeSettings: settings,
                        isMatchRunning: false,
                        onTimeExpired,
                        matchEndedRef,
                    }),
                { initialProps: { settings: initialSettings } },
            );

            const newSettings: ClockSettings = {
                sente: { mainMs: 200000, byoyomiMs: 10000 },
                gote: { mainMs: 300000, byoyomiMs: 15000 },
            };

            rerender({ settings: newSettings });

            act(() => {
                result.current.resetClocks(false);
            });

            expect(result.current.clocks.sente.mainMs).toBe(200000);
            expect(result.current.clocks.sente.byoyomiMs).toBe(10000);
            expect(result.current.clocks.gote.mainMs).toBe(300000);
            expect(result.current.clocks.gote.byoyomiMs).toBe(15000);
        });
    });
});
