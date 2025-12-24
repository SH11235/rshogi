import { describe, expect, it } from "vitest";
import { dropTargetEquals } from "./hitDetection";
import type { DropTarget } from "./types";

/**
 * hitDetection のテスト
 *
 * 注: hitTestBoard, hitTestZones, getDropTarget は document.elementFromPoint() を使用するため、
 * 実際の DOM が必要です。これらのテストは E2E テストまたは統合テストで行います。
 *
 * このファイルでは純粋関数の dropTargetEquals のみをテストします。
 */
describe("hitDetection", () => {
    describe("dropTargetEquals", () => {
        it("両方 null なら true", () => {
            expect(dropTargetEquals(null, null)).toBe(true);
        });

        it("片方だけ null なら false", () => {
            const target: DropTarget = { type: "board", square: "5e" };
            expect(dropTargetEquals(target, null)).toBe(false);
            expect(dropTargetEquals(null, target)).toBe(false);
        });

        it("異なる type なら false", () => {
            const a: DropTarget = { type: "board", square: "5e" };
            const b: DropTarget = { type: "delete" };
            expect(dropTargetEquals(a, b)).toBe(false);
        });

        it("同じ board マスなら true", () => {
            const a: DropTarget = { type: "board", square: "5e" };
            const b: DropTarget = { type: "board", square: "5e" };
            expect(dropTargetEquals(a, b)).toBe(true);
        });

        it("異なる board マスなら false", () => {
            const a: DropTarget = { type: "board", square: "5e" };
            const b: DropTarget = { type: "board", square: "5d" };
            expect(dropTargetEquals(a, b)).toBe(false);
        });

        it("同じ hand owner なら true", () => {
            const a: DropTarget = { type: "hand", owner: "sente" };
            const b: DropTarget = { type: "hand", owner: "sente" };
            expect(dropTargetEquals(a, b)).toBe(true);
        });

        it("異なる hand owner なら false", () => {
            const a: DropTarget = { type: "hand", owner: "sente" };
            const b: DropTarget = { type: "hand", owner: "gote" };
            expect(dropTargetEquals(a, b)).toBe(false);
        });

        it("両方 delete なら true", () => {
            const a: DropTarget = { type: "delete" };
            const b: DropTarget = { type: "delete" };
            expect(dropTargetEquals(a, b)).toBe(true);
        });
    });
});
