import { minutesToMs, secondsToMs } from "@/types/timer";
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useTimerStore } from "./timerStore";

// Mock timers
vi.useFakeTimers();

describe("timerStore", () => {
    beforeEach(() => {
        // Reset store to initial state
        const store = useTimerStore.getState();
        store.reset();
        store.updateConfig({
            mainTimeMs: minutesToMs(10),
            byoyomiMs: secondsToMs(30),
            enabled: false,
        });
        vi.clearAllTimers();
    });

    describe("initial state", () => {
        it("should have correct initial state", () => {
            const { result } = renderHook(() => useTimerStore());

            expect(result.current.blackMainTime).toBe(minutesToMs(10));
            expect(result.current.whiteMainTime).toBe(minutesToMs(10));
            expect(result.current.byoyomiTime).toBeNull();
            expect(result.current.isRunning).toBe(false);
            expect(result.current.activePlayer).toBeNull();
            expect(result.current.hasTimedOut).toBe(false);
            expect(result.current.timedOutPlayer).toBeNull();
        });
    });

    describe("configuration", () => {
        it("should update configuration", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({
                    mainTimeMs: minutesToMs(5),
                    byoyomiMs: secondsToMs(10),
                    enabled: true,
                });
            });

            expect(result.current.config.mainTimeMs).toBe(minutesToMs(5));
            expect(result.current.config.byoyomiMs).toBe(secondsToMs(10));
            expect(result.current.config.enabled).toBe(true);
            expect(result.current.blackMainTime).toBe(minutesToMs(5));
            expect(result.current.whiteMainTime).toBe(minutesToMs(5));
        });
    });

    describe("timer control", () => {
        it("should start timer when enabled", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({ enabled: true });
                result.current.start("black");
            });

            expect(result.current.isRunning).toBe(true);
            expect(result.current.activePlayer).toBe("black");
        });

        it("should not start timer when disabled", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({ enabled: false });
                result.current.start("black");
            });

            expect(result.current.isRunning).toBe(false);
            expect(result.current.activePlayer).toBeNull();
        });

        it("should pause timer", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({ enabled: true });
                result.current.start("black");
                result.current.pause();
            });

            expect(result.current.isRunning).toBe(false);
        });

        it("should resume timer", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({ enabled: true });
                result.current.start("black");
                result.current.pause();
                result.current.resume();
            });

            expect(result.current.isRunning).toBe(true);
        });

        it("should switch players", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({ enabled: true });
                result.current.start("black");
                result.current.switchPlayer();
            });

            expect(result.current.activePlayer).toBe("white");
            expect(result.current.byoyomiTime).toBeNull();
        });

        it("should reset timer", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({
                    enabled: true,
                    mainTimeMs: minutesToMs(5),
                });
                result.current.start("black");
            });

            act(() => {
                result.current.reset();
            });

            expect(result.current.isRunning).toBe(false);
            expect(result.current.activePlayer).toBeNull();
            expect(result.current.blackMainTime).toBe(minutesToMs(5));
            expect(result.current.whiteMainTime).toBe(minutesToMs(5));
            expect(result.current.byoyomiTime).toBeNull();
        });
    });

    describe("countdown behavior", () => {
        it("should count down main time", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({
                    enabled: true,
                    mainTimeMs: secondsToMs(5),
                });
                result.current.start("black");
            });

            const initialTime = result.current.blackMainTime;

            // Simulate 1 second passing
            act(() => {
                vi.advanceTimersByTime(1000);
                result.current.tick();
            });

            expect(result.current.blackMainTime).toBeLessThan(initialTime);
        });

        it("should enter byoyomi when main time expires", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({
                    enabled: true,
                    mainTimeMs: 1000, // 1 second
                    byoyomiMs: 5000, // 5 seconds
                });
                result.current.start("black");
            });

            // Simulate main time expiring
            act(() => {
                vi.advanceTimersByTime(1100);
                result.current.tick();
            });

            expect(result.current.blackMainTime).toBe(0);
            expect(result.current.byoyomiTime).toBe(5000);
        });

        it("should timeout when byoyomi expires", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({
                    enabled: true,
                    mainTimeMs: 1000,
                    byoyomiMs: 1000, // 1 second byoyomi
                });
                result.current.start("black");
            });

            // Simulate main time expiring
            act(() => {
                vi.advanceTimersByTime(1100);
                result.current.tick();
            });

            // Simulate byoyomi expiring
            act(() => {
                vi.advanceTimersByTime(1100);
                result.current.tick();
            });

            expect(result.current.hasTimedOut).toBe(true);
            expect(result.current.timedOutPlayer).toBe("black");
            expect(result.current.isRunning).toBe(false);
        });

        it("should timeout immediately when no byoyomi", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({
                    enabled: true,
                    mainTimeMs: 1000,
                    byoyomiMs: 0, // No byoyomi
                });
                result.current.start("black");
            });

            // Simulate main time expiring
            act(() => {
                vi.advanceTimersByTime(1100);
                result.current.tick();
            });

            expect(result.current.blackMainTime).toBe(0);
            expect(result.current.hasTimedOut).toBe(true);
            expect(result.current.timedOutPlayer).toBe("black");
        });
    });

    describe("edge cases", () => {
        it("should not tick when disabled", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({ enabled: false });
                result.current.start("black");
            });

            const initialTime = result.current.blackMainTime;

            act(() => {
                vi.advanceTimersByTime(1000);
                result.current.tick();
            });

            expect(result.current.blackMainTime).toBe(initialTime);
        });

        it("should not tick when paused", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({ enabled: true });
                result.current.start("black");
                result.current.pause();
            });

            const initialTime = result.current.blackMainTime;

            act(() => {
                vi.advanceTimersByTime(1000);
                result.current.tick();
            });

            expect(result.current.blackMainTime).toBe(initialTime);
        });

        it("should handle manual timeout", () => {
            const { result } = renderHook(() => useTimerStore());

            act(() => {
                result.current.updateConfig({ enabled: true });
                result.current.start("black");
                result.current.handleTimeout("black");
            });

            expect(result.current.hasTimedOut).toBe(true);
            expect(result.current.timedOutPlayer).toBe("black");
            expect(result.current.isRunning).toBe(false);
        });
    });
});
