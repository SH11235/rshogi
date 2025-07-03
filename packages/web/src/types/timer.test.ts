import { describe, expect, it } from "vitest";
import {
    formatByoyomiTime,
    formatConsiderationTime,
    formatTimeWithHours,
    getWarningLevel,
} from "./timer";

describe("Timer Helper Functions", () => {
    describe("getWarningLevel", () => {
        it("should return critical for byoyomi", () => {
            expect(getWarningLevel(30000, true)).toBe("critical");
            expect(getWarningLevel(5000, true)).toBe("critical");
        });

        it("should return critical for time <= 10 seconds", () => {
            expect(getWarningLevel(10000, false)).toBe("critical");
            expect(getWarningLevel(5000, false)).toBe("critical");
            expect(getWarningLevel(1000, false)).toBe("critical");
        });

        it("should return warning for time <= 30 seconds", () => {
            expect(getWarningLevel(30000, false)).toBe("warning");
            expect(getWarningLevel(20000, false)).toBe("warning");
            expect(getWarningLevel(15000, false)).toBe("warning");
        });

        it("should return normal for time > 30 seconds", () => {
            expect(getWarningLevel(31000, false)).toBe("normal");
            expect(getWarningLevel(60000, false)).toBe("normal");
            expect(getWarningLevel(600000, false)).toBe("normal");
        });
    });

    describe("formatTimeWithHours", () => {
        it("should format time without hours for < 1 hour", () => {
            expect(formatTimeWithHours(0)).toBe("00:00");
            expect(formatTimeWithHours(5000)).toBe("00:05");
            expect(formatTimeWithHours(65000)).toBe("01:05");
            expect(formatTimeWithHours(3599000)).toBe("59:59");
        });

        it("should format time with hours for >= 1 hour", () => {
            expect(formatTimeWithHours(3600000)).toBe("1:00:00");
            expect(formatTimeWithHours(3665000)).toBe("1:01:05");
            expect(formatTimeWithHours(7200000)).toBe("2:00:00");
            expect(formatTimeWithHours(10925000)).toBe("3:02:05");
        });
    });

    describe("formatByoyomiTime", () => {
        it("should format byoyomi time correctly", () => {
            expect(formatByoyomiTime(0)).toBe("秒読み 0");
            expect(formatByoyomiTime(1000)).toBe("秒読み 1");
            expect(formatByoyomiTime(10000)).toBe("秒読み 10");
            expect(formatByoyomiTime(30000)).toBe("秒読み 30");
            expect(formatByoyomiTime(60000)).toBe("秒読み 60");
        });

        it("should truncate milliseconds", () => {
            expect(formatByoyomiTime(1500)).toBe("秒読み 1");
            expect(formatByoyomiTime(10999)).toBe("秒読み 10");
        });
    });

    describe("formatConsiderationTime", () => {
        it("should format consideration count correctly", () => {
            expect(formatConsiderationTime(0)).toBe("考慮時間 残り0回");
            expect(formatConsiderationTime(1)).toBe("考慮時間 残り1回");
            expect(formatConsiderationTime(5)).toBe("考慮時間 残り5回");
            expect(formatConsiderationTime(10)).toBe("考慮時間 残り10回");
        });
    });
});
