import { describe, expect, it } from "vitest";
import { formatTime, getOtherPlayer, minutesToMs, secondsToMs } from "./timer";

describe("timer utilities", () => {
    describe("minutesToMs", () => {
        it("should convert minutes to milliseconds", () => {
            expect(minutesToMs(1)).toBe(60000);
            expect(minutesToMs(5)).toBe(300000);
            expect(minutesToMs(0.5)).toBe(30000);
        });
    });

    describe("secondsToMs", () => {
        it("should convert seconds to milliseconds", () => {
            expect(secondsToMs(1)).toBe(1000);
            expect(secondsToMs(30)).toBe(30000);
            expect(secondsToMs(0.5)).toBe(500);
        });
    });

    describe("formatTime", () => {
        it("should format time correctly", () => {
            expect(formatTime(0)).toBe("0:00");
            expect(formatTime(1000)).toBe("0:01");
            expect(formatTime(30000)).toBe("0:30");
            expect(formatTime(60000)).toBe("1:00");
            expect(formatTime(90000)).toBe("1:30");
            expect(formatTime(3600000)).toBe("60:00");
        });

        it("should handle negative time", () => {
            expect(formatTime(-1000)).toBe("0:00");
        });

        it("should round up fractional seconds", () => {
            expect(formatTime(1500)).toBe("0:02"); // 1.5 seconds rounds up to 2
            expect(formatTime(999)).toBe("0:01"); // 0.999 seconds rounds up to 1
        });
    });

    describe("getOtherPlayer", () => {
        it("should return the opposite player", () => {
            expect(getOtherPlayer("black")).toBe("white");
            expect(getOtherPlayer("white")).toBe("black");
        });
    });
});
