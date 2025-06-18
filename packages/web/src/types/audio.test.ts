import { describe, expect, it } from "vitest";
import { floatToVolumeLevel, volumeLevelToFloat } from "./audio";

describe("audio utilities", () => {
    describe("volumeLevelToFloat", () => {
        it("should convert volume level to float correctly", () => {
            expect(volumeLevelToFloat(0)).toBe(0);
            expect(volumeLevelToFloat(25)).toBe(0.25);
            expect(volumeLevelToFloat(50)).toBe(0.5);
            expect(volumeLevelToFloat(75)).toBe(0.75);
            expect(volumeLevelToFloat(100)).toBe(1);
        });
    });

    describe("floatToVolumeLevel", () => {
        it("should convert float to volume level correctly", () => {
            expect(floatToVolumeLevel(0)).toBe(0);
            expect(floatToVolumeLevel(0.25)).toBe(25);
            expect(floatToVolumeLevel(0.5)).toBe(50);
            expect(floatToVolumeLevel(0.75)).toBe(75);
            expect(floatToVolumeLevel(1)).toBe(100);
        });

        it("should clamp values to valid range", () => {
            expect(floatToVolumeLevel(-0.5)).toBe(0);
            expect(floatToVolumeLevel(1.5)).toBe(100);
        });

        it("should round to nearest integer", () => {
            expect(floatToVolumeLevel(0.234)).toBe(23);
            expect(floatToVolumeLevel(0.236)).toBe(24);
            expect(floatToVolumeLevel(0.789)).toBe(79);
        });
    });
});
