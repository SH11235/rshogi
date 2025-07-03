import { useGameStore } from "@/stores/gameStore";
import type { TimerState } from "@/types/timer";
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { Timer } from "./Timer";

// useGameStoreのモック
vi.mock("@/stores/gameStore");

const mockUseGameStore = vi.mocked(useGameStore);

// Timer コンポーネントに必要な GameStore の部分的な型
type PartialGameStore = {
    timer: TimerState;
    tick: () => void;
};

describe("Timer", () => {
    const mockTick = vi.fn();

    const createTimerState = (overrides: Partial<TimerState> = {}): TimerState => ({
        config: {
            mode: "basic",
            basicTime: 600,
            byoyomiTime: 30,
            fischerIncrement: 0,
            perMoveLimit: 0,
            considerationTime: 0,
            considerationCount: 0,
        },
        blackTime: 600000, // 10 minutes in ms
        whiteTime: 600000,
        blackInByoyomi: false,
        whiteInByoyomi: false,
        blackConsiderationsRemaining: 0,
        whiteConsiderationsRemaining: 0,
        isUsingConsideration: false,
        considerationStartTime: null,
        activePlayer: null,
        isPaused: false,
        lastTickTime: Date.now(),
        blackWarningLevel: "normal",
        whiteWarningLevel: "normal",
        hasTimedOut: false,
        timedOutPlayer: null,
        ...overrides,
    });

    beforeEach(() => {
        vi.clearAllMocks();
    });

    it("should not render when timer is disabled", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({
                config: {
                    mode: null,
                    basicTime: 0,
                    byoyomiTime: 0,
                    fischerIncrement: 0,
                    perMoveLimit: 0,
                    considerationTime: 0,
                    considerationCount: 0,
                },
            }),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="black" />);

        expect(screen.queryByText("先手")).not.toBeInTheDocument();
    });

    it("should render black player timer", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState(),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="black" />);

        expect(screen.getByText("先手")).toBeInTheDocument();
        expect(screen.getByText("10:00.0")).toBeInTheDocument();
    });

    it("should render white player timer", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState(),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="white" />);

        expect(screen.getByText("後手")).toBeInTheDocument();
        expect(screen.getByText("10:00.0")).toBeInTheDocument();
    });

    it("should show active state", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({ activePlayer: "black" }),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="black" />);

        const card = screen.getByText("先手").closest(".transition-all");
        expect(card).toHaveClass("bg-primary", "text-primary-foreground");
    });

    it("should show byoyomi time when in byoyomi", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({ blackTime: 30000, blackInByoyomi: true }),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="black" />);

        expect(screen.getByText("00:30.0")).toBeInTheDocument();
        expect(screen.getByText(/秒読み/)).toBeInTheDocument();
    });

    it("should apply warning styling", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({
                blackTime: 25000,
                blackWarningLevel: "warning",
                activePlayer: "black",
            }),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="black" />);

        const card = screen.getByText("先手").closest(".transition-all");
        expect(card).toHaveClass("border-yellow-500");
    });

    it("should apply critical time styling with animation", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({
                blackTime: 5000,
                blackWarningLevel: "critical",
                activePlayer: "black",
            }),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="black" />);

        const timeDisplay = screen.getByText("00:05.0");
        expect(timeDisplay).toHaveClass("text-red-500");
        const card = screen.getByText("先手").closest(".transition-all");
        expect(card).toHaveClass("border-red-500", "animate-pulse");
    });

    it("should format time correctly", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({ blackTime: 90000 }),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="black" />);

        expect(screen.getByText("01:30.0")).toBeInTheDocument();
    });

    it("should show per move format", () => {
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({
                config: {
                    mode: "perMove",
                    basicTime: 0,
                    byoyomiTime: 0,
                    fischerIncrement: 0,
                    perMoveLimit: 60,
                    considerationTime: 0,
                    considerationCount: 0,
                },
                blackTime: 45300,
            }),
            tick: mockTick,
        } as unknown as PartialGameStore);

        render(<Timer player="black" />);

        expect(screen.getByText("45.3秒")).toBeInTheDocument();
    });

    it("should show correct time in byoyomi vs main time", () => {
        const { rerender } = render(<Timer player="black" />);

        // Main time
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({ blackTime: 300000, blackInByoyomi: false }),
            tick: mockTick,
        } as unknown as PartialGameStore);
        rerender(<Timer player="black" />);

        expect(screen.getByText("05:00.0")).toBeInTheDocument();

        // Byoyomi time
        mockUseGameStore.mockReturnValue({
            timer: createTimerState({ blackTime: 15000, blackInByoyomi: true }),
            tick: mockTick,
        } as unknown as PartialGameStore);
        rerender(<Timer player="black" />);

        expect(screen.getByText("00:15.0")).toBeInTheDocument();
        expect(screen.getByText(/秒読み/)).toBeInTheDocument();
    });
});
