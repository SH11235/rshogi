import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { TimerDisplay } from "./TimerDisplay";

// Mock the useIntegratedTimer hook
vi.mock("@/hooks/useIntegratedTimer", () => ({
    useIntegratedTimer: vi.fn(),
}));

// Mock the Timer component
vi.mock("./Timer", () => ({
    Timer: vi.fn(({ player, isActive, hasTimedOut, timedOutPlayer }) => (
        <div data-testid={`timer-${player}`}>
            Timer-{player}-{isActive ? "active" : "inactive"}-
            {hasTimedOut && timedOutPlayer === player ? "timeout" : "normal"}
        </div>
    )),
}));

import { useIntegratedTimer } from "@/hooks/useIntegratedTimer";

const mockUseIntegratedTimer = useIntegratedTimer as ReturnType<typeof vi.fn>;

describe("TimerDisplay", () => {
    const defaultTimerState = {
        blackMainTime: 600000, // 10 minutes
        whiteMainTime: 600000,
        byoyomiTime: null,
        isRunning: false,
        activePlayer: null,
        hasTimedOut: false,
        timedOutPlayer: null,
        config: {
            enabled: true,
            mainTimeMs: 600000,
            byoyomiMs: 30000,
        },
        manualStart: vi.fn(),
        manualPause: vi.fn(),
        manualResume: vi.fn(),
        manualReset: vi.fn(),
        manualSwitchPlayer: vi.fn(),
    };

    beforeEach(() => {
        mockUseIntegratedTimer.mockReturnValue(defaultTimerState);
    });

    it("should not render when timer is disabled", () => {
        mockUseIntegratedTimer.mockReturnValue({
            ...defaultTimerState,
            config: { ...defaultTimerState.config, enabled: false },
        });

        render(<TimerDisplay />);

        expect(screen.queryByText("⏱️ 対局時計")).not.toBeInTheDocument();
    });

    it("should render timer display when enabled", () => {
        render(<TimerDisplay />);

        expect(screen.getByText("⏱️ 対局時計")).toBeInTheDocument();
        expect(screen.getByTestId("timer-black")).toBeInTheDocument();
        expect(screen.getByTestId("timer-white")).toBeInTheDocument();
    });

    it("should show timer configuration", () => {
        render(<TimerDisplay />);

        expect(screen.getByText("持ち時間: 10分")).toBeInTheDocument();
        expect(screen.getByText("/ 秒読み: 30秒")).toBeInTheDocument();
    });

    it("should show timer configuration without byoyomi", () => {
        mockUseIntegratedTimer.mockReturnValue({
            ...defaultTimerState,
            config: { ...defaultTimerState.config, byoyomiMs: 0 },
        });

        render(<TimerDisplay />);

        expect(screen.getByText("持ち時間: 10分")).toBeInTheDocument();
        expect(screen.queryByText("/ 秒読み: 30秒")).not.toBeInTheDocument();
    });

    it("should show timeout message", () => {
        mockUseIntegratedTimer.mockReturnValue({
            ...defaultTimerState,
            hasTimedOut: true,
            timedOutPlayer: "black",
        });

        render(<TimerDisplay />);

        expect(screen.getByText("先手の時間切れ")).toBeInTheDocument();
    });

    it("should show timeout message for white player", () => {
        mockUseIntegratedTimer.mockReturnValue({
            ...defaultTimerState,
            hasTimedOut: true,
            timedOutPlayer: "white",
        });

        render(<TimerDisplay />);

        expect(screen.getByText("後手の時間切れ")).toBeInTheDocument();
    });

    it("should pass correct props to Timer components", () => {
        mockUseIntegratedTimer.mockReturnValue({
            ...defaultTimerState,
            isRunning: true,
            activePlayer: "black",
        });

        render(<TimerDisplay />);

        expect(screen.getByTestId("timer-black")).toHaveTextContent("Timer-black-active-normal");
        expect(screen.getByTestId("timer-white")).toHaveTextContent("Timer-white-inactive-normal");
    });

    it("should pass timeout state to Timer components", () => {
        mockUseIntegratedTimer.mockReturnValue({
            ...defaultTimerState,
            hasTimedOut: true,
            timedOutPlayer: "white",
        });

        render(<TimerDisplay />);

        expect(screen.getByTestId("timer-black")).toHaveTextContent("Timer-black-inactive-normal");
        expect(screen.getByTestId("timer-white")).toHaveTextContent("Timer-white-inactive-timeout");
    });

    it("should call onTimeout callback", () => {
        const onTimeout = vi.fn();

        render(<TimerDisplay onTimeout={onTimeout} />);

        // The hook should be called with the onTimeout callback
        expect(mockUseIntegratedTimer).toHaveBeenCalledWith({ onTimeout });
    });

    // Note: Development controls are only shown in NODE_ENV === 'development'
    // but since we're in test environment, they won't be rendered
    it("should not show development controls in test environment", () => {
        render(<TimerDisplay />);

        expect(screen.queryByText("開発用コントロール")).not.toBeInTheDocument();
    });
});
