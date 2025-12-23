import { describe, expect, it } from "vitest";
import { dropTargetEquals, getDropTarget, hitTestBoard, hitTestZones } from "./hit-test";
import type { BoardMetrics, DropTarget, Zones } from "./types";

describe("hit-test", () => {
    describe("hitTestBoard", () => {
        // テスト用の盤面メトリクス（100x100pxの盤面を想定）
        const createMetrics = (orientation: "sente" | "gote"): BoardMetrics => ({
            rect: {
                left: 0,
                top: 0,
                right: 90,
                bottom: 90,
                width: 90,
                height: 90,
            } as DOMRect,
            cellW: 10,
            cellH: 10,
            orientation,
        });

        describe("先手視点 (orientation = sente)", () => {
            const metrics = createMetrics("sente");

            it("左上のマス(9a)を正しく判定する", () => {
                // 左上は9a（col=0, row=0）
                const result = hitTestBoard(5, 5, metrics);
                expect(result).toBe("9a");
            });

            it("右下のマス(1i)を正しく判定する", () => {
                // 右下は1i（col=8, row=8）
                const result = hitTestBoard(85, 85, metrics);
                expect(result).toBe("1i");
            });

            it("中央のマス(5e)を正しく判定する", () => {
                // 中央は5e（col=4, row=4）
                const result = hitTestBoard(45, 45, metrics);
                expect(result).toBe("5e");
            });

            it("セルの境界で正しく判定する", () => {
                // col=0の右端 (x=9.9) はまだ9筋
                const result1 = hitTestBoard(9.9, 5, metrics);
                expect(result1).toBe("9a");

                // col=1の左端 (x=10) は8筋
                const result2 = hitTestBoard(10, 5, metrics);
                expect(result2).toBe("8a");
            });

            it("盤外を正しく判定する", () => {
                expect(hitTestBoard(-1, 5, metrics)).toBeNull();
                expect(hitTestBoard(91, 5, metrics)).toBeNull();
                expect(hitTestBoard(5, -1, metrics)).toBeNull();
                expect(hitTestBoard(5, 91, metrics)).toBeNull();
            });
        });

        describe("後手視点 (orientation = gote)", () => {
            const metrics = createMetrics("gote");

            it("左上のマス(1i)を正しく判定する", () => {
                // 後手視点では左上が1i
                const result = hitTestBoard(5, 5, metrics);
                expect(result).toBe("1i");
            });

            it("右下のマス(9a)を正しく判定する", () => {
                // 後手視点では右下が9a
                const result = hitTestBoard(85, 85, metrics);
                expect(result).toBe("9a");
            });

            it("中央のマス(5e)を正しく判定する", () => {
                // 中央は先手でも後手でも5e
                const result = hitTestBoard(45, 45, metrics);
                expect(result).toBe("5e");
            });
        });
    });

    describe("hitTestZones", () => {
        const createZones = (): Zones => ({
            senteHandRect: {
                left: 0,
                top: 100,
                right: 50,
                bottom: 130,
            } as DOMRect,
            goteHandRect: {
                left: 50,
                top: 100,
                right: 100,
                bottom: 130,
            } as DOMRect,
            deleteRect: {
                left: 0,
                top: 140,
                right: 100,
                bottom: 170,
            } as DOMRect,
        });

        it("削除ゾーンを正しく判定する", () => {
            const zones = createZones();
            const result = hitTestZones(50, 155, zones);
            expect(result).toEqual({ type: "delete" });
        });

        it("先手の持ち駒エリアを正しく判定する", () => {
            const zones = createZones();
            const result = hitTestZones(25, 115, zones);
            expect(result).toEqual({ type: "hand", owner: "sente" });
        });

        it("後手の持ち駒エリアを正しく判定する", () => {
            const zones = createZones();
            const result = hitTestZones(75, 115, zones);
            expect(result).toEqual({ type: "hand", owner: "gote" });
        });

        it("どのゾーンにも該当しない場合はnullを返す", () => {
            const zones = createZones();
            const result = hitTestZones(50, 50, zones);
            expect(result).toBeNull();
        });

        it("nullのゾーンは無視される", () => {
            const zones: Zones = {
                senteHandRect: null,
                goteHandRect: null,
                deleteRect: null,
            };
            const result = hitTestZones(50, 115, zones);
            expect(result).toBeNull();
        });
    });

    describe("getDropTarget", () => {
        const boardMetrics: BoardMetrics = {
            rect: {
                left: 0,
                top: 0,
                right: 90,
                bottom: 90,
                width: 90,
                height: 90,
            } as DOMRect,
            cellW: 10,
            cellH: 10,
            orientation: "sente",
        };

        const zones: Zones = {
            senteHandRect: {
                left: 0,
                top: 100,
                right: 50,
                bottom: 130,
            } as DOMRect,
            goteHandRect: {
                left: 50,
                top: 100,
                right: 100,
                bottom: 130,
            } as DOMRect,
            deleteRect: {
                left: 0,
                top: 140,
                right: 100,
                bottom: 170,
            } as DOMRect,
        };

        it("削除ゾーンが最優先される", () => {
            const result = getDropTarget(50, 155, boardMetrics, zones);
            expect(result).toEqual({ type: "delete" });
        });

        it("盤上のマスを正しく返す", () => {
            const result = getDropTarget(45, 45, boardMetrics, zones);
            expect(result).toEqual({ type: "board", square: "5e" });
        });

        it("持ち駒エリアを正しく返す", () => {
            const result = getDropTarget(25, 115, boardMetrics, zones);
            expect(result).toEqual({ type: "hand", owner: "sente" });
        });

        it("エリア外は delete を返す（outsideAreaBehavior = delete）", () => {
            const result = getDropTarget(200, 200, boardMetrics, zones, "delete");
            expect(result).toEqual({ type: "delete" });
        });

        it("エリア外は null を返す（outsideAreaBehavior = cancel）", () => {
            const result = getDropTarget(200, 200, boardMetrics, zones, "cancel");
            expect(result).toBeNull();
        });
    });

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
