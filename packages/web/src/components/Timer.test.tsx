import { minutesToMs, secondsToMs } from "@/types/timer";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Timer } from "./Timer";

describe("Timer", () => {
    const defaultProps = {
        player: "black" as const,
        mainTime: minutesToMs(10),
        byoyomiTime: null,
        isActive: false,
        isEnabled: true,
        hasTimedOut: false,
        timedOutPlayer: null,
    };

    it("should not render when timer is disabled", () => {
        render(<Timer {...defaultProps} isEnabled={false} />);

        expect(screen.queryByText("先手")).not.toBeInTheDocument();
    });

    it("should render black player timer", () => {
        render(<Timer {...defaultProps} player="black" />);

        expect(screen.getByText("先手")).toBeInTheDocument();
        expect(screen.getByText("10:00")).toBeInTheDocument();
    });

    it("should render white player timer", () => {
        render(<Timer {...defaultProps} player="white" />);

        expect(screen.getByText("後手")).toBeInTheDocument();
        expect(screen.getByText("10:00")).toBeInTheDocument();
    });

    it("should show active state", () => {
        render(<Timer {...defaultProps} isActive={true} />);

        const container = screen.getByText("先手").closest("div")?.parentElement;
        expect(container).toHaveClass("ring-2", "ring-blue-500");
    });

    it("should show byoyomi time when in byoyomi", () => {
        render(<Timer {...defaultProps} byoyomiTime={secondsToMs(30)} />);

        expect(screen.getByText("0:30")).toBeInTheDocument();
        expect(screen.getByText("秒読み")).toBeInTheDocument();
    });

    it("should show timeout state", () => {
        render(
            <Timer {...defaultProps} hasTimedOut={true} timedOutPlayer="black" player="black" />,
        );

        expect(screen.getByText("時間切れ")).toBeInTheDocument();
        const container = screen.getByText("先手").closest("div")?.parentElement;
        expect(container).toHaveClass("ring-red-500");
    });

    it("should show main time exhausted indicator", () => {
        render(<Timer {...defaultProps} mainTime={0} byoyomiTime={null} />);

        expect(screen.getByText("本時終了")).toBeInTheDocument();
    });

    it("should apply low time styling", () => {
        render(<Timer {...defaultProps} mainTime={secondsToMs(25)} isActive={true} />);

        const timeDisplay = screen.getByText("0:25");
        expect(timeDisplay).toHaveClass("text-orange-500");
    });

    it("should apply critical time styling with animation", () => {
        render(<Timer {...defaultProps} mainTime={secondsToMs(5)} isActive={true} />);

        const timeDisplay = screen.getByText("0:05");
        expect(timeDisplay).toHaveClass("text-red-500", "animate-pulse");
    });

    it("should format time correctly", () => {
        render(<Timer {...defaultProps} mainTime={secondsToMs(90)} />);

        expect(screen.getByText("1:30")).toBeInTheDocument();
    });

    it("should show correct time in byoyomi vs main time", () => {
        const { rerender } = render(
            <Timer {...defaultProps} mainTime={minutesToMs(5)} byoyomiTime={null} />,
        );

        expect(screen.getByText("5:00")).toBeInTheDocument();

        rerender(<Timer {...defaultProps} mainTime={0} byoyomiTime={secondsToMs(15)} />);

        expect(screen.getByText("0:15")).toBeInTheDocument();
        expect(screen.getByText("秒読み")).toBeInTheDocument();
    });
});
