import { describe, expect, it } from "vitest";
import type { NnueMeta, PresetConfig } from "@shogi/app-core";
import {
    buildNnueOptions,
    NNUE_VALUE_PREFIX,
    parseNnueSelectionValue,
    toNnueSelectionValue,
    toOptionValue,
} from "./nnueSelectionUtils";

describe("nnueSelectionUtils", () => {
    describe("buildNnueOptions", () => {
        const mockPresets: PresetConfig[] = [
            {
                presetKey: "ramu",
                displayName: "らむ",
                description: "らむNNUE",
                url: "https://example.com/ramu.bin",
                sha256: "abc123",
                size: 1000000,
                license: "MIT",
                releasedAt: "2024-01-01",
            },
            {
                presetKey: "tanuki",
                displayName: "たぬき",
                description: "たぬきNNUE",
                url: "https://example.com/tanuki.bin",
                sha256: "def456",
                size: 2000000,
                license: "MIT",
                releasedAt: "2024-01-01",
            },
        ];

        const mockNnueList: NnueMeta[] = [
            {
                id: "preset-ramu-id",
                displayName: "らむ",
                originalFileName: "ramu.bin",
                size: 1000000,
                contentHashSha256: "abc123",
                source: "preset",
                presetKey: "ramu",
                createdAt: Date.now(),
                verified: true,
            },
            {
                id: "custom-nnue-id",
                displayName: "カスタムNNUE",
                originalFileName: "custom.bin",
                size: 500000,
                contentHashSha256: "xyz789",
                source: "user-uploaded",
                createdAt: Date.now(),
                verified: true,
            },
        ];

        it("プリセット＋カスタムNNUEから選択肢を構築", () => {
            const options = buildNnueOptions({
                presets: mockPresets,
                nnueList: mockNnueList,
            });

            expect(options).toHaveLength(3);
            expect(options[0]).toEqual({
                type: "preset",
                key: "ramu",
                label: "らむ", // DL済みなので (要DL) なし
            });
            expect(options[1]).toEqual({
                type: "preset",
                key: "tanuki",
                label: "たぬき (要DL)", // 未DLなので (要DL) あり
            });
            expect(options[2]).toEqual({
                type: "custom",
                key: "custom-nnue-id",
                label: "カスタムNNUE",
            });
        });

        it("DL済みプリセットにはラベルに(要DL)なし", () => {
            const options = buildNnueOptions({
                presets: mockPresets,
                nnueList: mockNnueList,
            });

            const ramuOption = options.find((o) => o.key === "ramu");
            expect(ramuOption?.label).toBe("らむ");
            expect(ramuOption?.label).not.toContain("(要DL)");
        });

        it("未DLプリセットにはラベルに(要DL)あり", () => {
            const options = buildNnueOptions({
                presets: mockPresets,
                nnueList: mockNnueList,
            });

            const tanukiOption = options.find((o) => o.key === "tanuki");
            expect(tanukiOption?.label).toBe("たぬき (要DL)");
        });

        it("プリセット空の場合はカスタムのみ", () => {
            const options = buildNnueOptions({
                presets: [],
                nnueList: mockNnueList,
            });

            expect(options).toHaveLength(1);
            expect(options[0].type).toBe("custom");
        });

        it("nnueList空の場合はプリセットのみ（全て要DL）", () => {
            const options = buildNnueOptions({
                presets: mockPresets,
                nnueList: [],
            });

            expect(options).toHaveLength(2);
            expect(options.every((o) => o.label.includes("(要DL)"))).toBe(true);
        });

        it("両方undefinedの場合は空配列", () => {
            const options = buildNnueOptions({
                presets: undefined,
                nnueList: undefined,
            });

            expect(options).toHaveLength(0);
        });
    });

    describe("toNnueSelectionValue", () => {
        it("preset選択 → preset:key", () => {
            expect(
                toNnueSelectionValue({
                    presetKey: "ramu",
                    nnueId: null,
                }),
            ).toBe("preset:ramu");
        });

        it("custom選択 → custom:id", () => {
            expect(
                toNnueSelectionValue({
                    presetKey: null,
                    nnueId: "abc123",
                }),
            ).toBe("custom:abc123");
        });

        it("両方nullの場合 → material", () => {
            expect(
                toNnueSelectionValue({
                    presetKey: null,
                    nnueId: null,
                }),
            ).toBe("material");
        });

        it("undefined → material", () => {
            expect(toNnueSelectionValue(undefined)).toBe("material");
        });

        it("presetKeyが優先される（両方設定されている場合）", () => {
            expect(
                toNnueSelectionValue({
                    presetKey: "ramu",
                    nnueId: "abc123",
                }),
            ).toBe("preset:ramu");
        });
    });

    describe("parseNnueSelectionValue", () => {
        it("preset:key → {presetKey, nnueId:null}", () => {
            expect(parseNnueSelectionValue("preset:ramu")).toEqual({
                presetKey: "ramu",
                nnueId: null,
            });
        });

        it("custom:id → {presetKey:null, nnueId}", () => {
            expect(parseNnueSelectionValue("custom:abc123")).toEqual({
                presetKey: null,
                nnueId: "abc123",
            });
        });

        it("material → {presetKey:null, nnueId:null}", () => {
            expect(parseNnueSelectionValue("material")).toEqual({
                presetKey: null,
                nnueId: null,
            });
        });

        it("不明な値 → {presetKey:null, nnueId:null}", () => {
            expect(parseNnueSelectionValue("unknown")).toEqual({
                presetKey: null,
                nnueId: null,
            });
        });

        it("空文字列 → {presetKey:null, nnueId:null}", () => {
            expect(parseNnueSelectionValue("")).toEqual({
                presetKey: null,
                nnueId: null,
            });
        });
    });

    describe("toOptionValue", () => {
        it("preset型のオプション", () => {
            expect(
                toOptionValue({
                    type: "preset",
                    key: "ramu",
                    label: "らむ",
                }),
            ).toBe("preset:ramu");
        });

        it("custom型のオプション", () => {
            expect(
                toOptionValue({
                    type: "custom",
                    key: "abc123",
                    label: "カスタム",
                }),
            ).toBe("custom:abc123");
        });
    });

    describe("NNUE_VALUE_PREFIX", () => {
        it("定数が正しく定義されている", () => {
            expect(NNUE_VALUE_PREFIX.MATERIAL).toBe("material");
            expect(NNUE_VALUE_PREFIX.PRESET).toBe("preset:");
            expect(NNUE_VALUE_PREFIX.CUSTOM).toBe("custom:");
        });
    });
});
