import { describe, expect, it, vi } from "vitest";
import { LegalMoveCache } from "./legalMoveCache";

describe("LegalMoveCache", () => {
    describe("isCached", () => {
        it("キャッシュがない場合は false を返す", () => {
            const cache = new LegalMoveCache();
            expect(cache.isCached(0)).toBe(false);
        });

        it("キャッシュが存在し、手数が一致する場合は true を返す", () => {
            const cache = new LegalMoveCache();
            cache.set(1, new Set(["7g7f"]));
            expect(cache.isCached(1)).toBe(true);
        });

        it("キャッシュが存在するが、手数が異なる場合は false を返す", () => {
            const cache = new LegalMoveCache();
            cache.set(1, new Set(["7g7f"]));
            expect(cache.isCached(2)).toBe(false);
        });
    });

    describe("getCached", () => {
        it("キャッシュがない場合は null を返す", () => {
            const cache = new LegalMoveCache();
            expect(cache.getCached()).toBeNull();
        });

        it("キャッシュが存在する場合は合法手のセットを返す", () => {
            const cache = new LegalMoveCache();
            const moves = new Set(["7g7f", "3c3d"]);
            cache.set(1, moves);
            expect(cache.getCached()).toBe(moves);
        });
    });

    describe("set", () => {
        it("合法手のセットをキャッシュに保存する", () => {
            const cache = new LegalMoveCache();
            const moves = new Set(["7g7f", "3c3d"]);
            cache.set(1, moves);

            expect(cache.isCached(1)).toBe(true);
            expect(cache.getCached()).toBe(moves);
        });

        it("既存のキャッシュを上書きする", () => {
            const cache = new LegalMoveCache();
            cache.set(1, new Set(["7g7f"]));
            cache.set(2, new Set(["3c3d"]));

            expect(cache.isCached(1)).toBe(false);
            expect(cache.isCached(2)).toBe(true);
        });
    });

    describe("clear", () => {
        it("キャッシュをクリアする", () => {
            const cache = new LegalMoveCache();
            cache.set(1, new Set(["7g7f"]));
            cache.clear();

            expect(cache.isCached(1)).toBe(false);
            expect(cache.getCached()).toBeNull();
        });

        it("空のキャッシュをクリアしてもエラーにならない", () => {
            const cache = new LegalMoveCache();
            expect(() => cache.clear()).not.toThrow();
        });
    });

    describe("getOrResolve", () => {
        it("キャッシュがない場合はリゾルバを呼び出す", async () => {
            const cache = new LegalMoveCache();
            const resolver = vi.fn(async () => ["7g7f", "3c3d"]);

            const result = await cache.getOrResolve(1, resolver);

            expect(resolver).toHaveBeenCalledWith(1);
            expect(result).toEqual(new Set(["7g7f", "3c3d"]));
        });

        it("キャッシュが存在する場合はリゾルバを呼び出さない", async () => {
            const cache = new LegalMoveCache();
            const moves = new Set(["7g7f", "3c3d"]);
            cache.set(1, moves);

            const resolver = vi.fn(async () => ["2g2f"]);
            const result = await cache.getOrResolve(1, resolver);

            expect(resolver).not.toHaveBeenCalled();
            expect(result).toBe(moves);
        });

        it("リゾルバの結果をキャッシュに保存する", async () => {
            const cache = new LegalMoveCache();
            const resolver = async () => ["7g7f", "3c3d"];

            await cache.getOrResolve(1, resolver);

            expect(cache.isCached(1)).toBe(true);
            expect(cache.getCached()).toEqual(new Set(["7g7f", "3c3d"]));
        });

        it("異なる手数で異なる結果を返す", async () => {
            const cache = new LegalMoveCache();
            const resolver1 = async () => ["7g7f"];
            const resolver2 = async () => ["3c3d"];

            const result1 = await cache.getOrResolve(1, resolver1);
            const result2 = await cache.getOrResolve(2, resolver2);

            expect(result1).toEqual(new Set(["7g7f"]));
            expect(result2).toEqual(new Set(["3c3d"]));
        });

        it("リゾルバが空配列を返す場合は空のセットをキャッシュする", async () => {
            const cache = new LegalMoveCache();
            const resolver = async () => [];

            const result = await cache.getOrResolve(1, resolver);

            expect(result).toEqual(new Set());
            expect(cache.getCached()).toEqual(new Set());
        });
    });
});
