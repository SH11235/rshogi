import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EngineEvent, EngineInfoEvent, SkillLevelSettings } from "./index";
import {
    createMockEngineClient,
    detectSkillPreset,
    ELO_MAX,
    ELO_MIN,
    normalizeSkillLevelSettings,
    SKILL_LEVEL_MAX,
    SKILL_LEVEL_MIN,
    validateSkillLevelSettings,
} from "./index";

describe("createMockEngineClient", () => {
    beforeEach(() => {
        vi.useFakeTimers();
    });

    afterEach(() => {
        vi.useRealTimers();
        vi.clearAllMocks();
    });

    it("init と dispose が正しく動作する", async () => {
        const client = createMockEngineClient();

        await expect(client.init()).resolves.toBeUndefined();
        await expect(client.dispose()).resolves.toBeUndefined();
    });

    describe("loadPosition", () => {
        it("局面を読み込める", async () => {
            const client = createMockEngineClient();

            await expect(client.loadPosition("startpos")).resolves.toBeUndefined();
            await expect(client.loadPosition("startpos", ["7g7f"])).resolves.toBeUndefined();
        });
    });

    describe("search", () => {
        it("探索を開始し SearchHandle を返す", async () => {
            const client = createMockEngineClient();

            const handle = await client.search({});

            expect(handle).toBeDefined();
            expect(handle.cancel).toBeInstanceOf(Function);
        });

        it("一定時間後に bestmove イベントを発行する", async () => {
            const client = createMockEngineClient();
            const events: EngineEvent[] = [];

            client.subscribe((event) => {
                events.push(event);
            });

            await client.search({});

            // タイマーを進めて info イベントを発火
            vi.advanceTimersByTime(10);
            expect(events).toHaveLength(1);
            expect(events[0].type).toBe("info");

            // タイマーを進めて bestmove イベントを発火
            vi.advanceTimersByTime(40);
            expect(events).toHaveLength(2);
            expect(events[1].type).toBe("bestmove");
            if (events[1].type === "bestmove") {
                expect(events[1].move).toBe("resign");
            }
        });

        it("SearchHandle の cancel で探索をキャンセルできる", async () => {
            const client = createMockEngineClient();
            const events: EngineEvent[] = [];

            client.subscribe((event) => {
                events.push(event);
            });

            const handle = await client.search({});

            // 即座にキャンセル
            await handle.cancel();

            // タイマーを進めてもイベントは発火されない
            vi.advanceTimersByTime(100);
            expect(events).toHaveLength(0);
        });

        it("複数の探索リクエストでは前のものがキャンセルされる", async () => {
            const client = createMockEngineClient();
            const events: EngineEvent[] = [];

            client.subscribe((event) => {
                events.push(event);
            });

            await client.search({});
            await client.search({}); // 2回目で最初の探索がキャンセルされる

            // タイマーを進める
            vi.advanceTimersByTime(10);
            expect(events).toHaveLength(1); // info イベントのみ（2回目の探索）

            vi.advanceTimersByTime(40);
            expect(events).toHaveLength(2); // bestmove イベント（2回目の探索）
        });
    });

    describe("subscribe", () => {
        it("イベントハンドラを登録できる", () => {
            const client = createMockEngineClient();
            const handler = vi.fn();

            const unsubscribe = client.subscribe(handler);

            expect(unsubscribe).toBeInstanceOf(Function);
        });

        it("複数のハンドラを登録できる", async () => {
            const client = createMockEngineClient();
            const handler1 = vi.fn();
            const handler2 = vi.fn();

            client.subscribe(handler1);
            client.subscribe(handler2);

            await client.search({});
            vi.advanceTimersByTime(10);

            expect(handler1).toHaveBeenCalled();
            expect(handler2).toHaveBeenCalled();
        });

        it("unsubscribe でハンドラを削除できる", async () => {
            const client = createMockEngineClient();
            const handler = vi.fn();

            const unsubscribe = client.subscribe(handler);
            unsubscribe();

            await client.search({});
            vi.advanceTimersByTime(100);

            expect(handler).not.toHaveBeenCalled();
        });
    });

    describe("stop", () => {
        it("実行中の探索を停止できる", async () => {
            const client = createMockEngineClient();
            const events: EngineEvent[] = [];

            client.subscribe((event) => {
                events.push(event);
            });

            await client.search({});
            await client.stop();

            // タイマーを進めてもイベントは発火されない
            vi.advanceTimersByTime(100);
            expect(events).toHaveLength(0);
        });
    });

    describe("setOption", () => {
        it("オプションを設定できる", async () => {
            const client = createMockEngineClient();

            await expect(client.setOption("threads", 4)).resolves.toBeUndefined();
            await expect(client.setOption("hash", 1024)).resolves.toBeUndefined();
            await expect(client.setOption("ponder", true)).resolves.toBeUndefined();
        });
    });

    describe("dispose", () => {
        it("dispose でリソースをクリーンアップする", async () => {
            const client = createMockEngineClient();
            const events: EngineEvent[] = [];

            client.subscribe((event) => {
                events.push(event);
            });

            await client.search({});
            await client.dispose();

            // dispose 後はイベントが発火されない
            vi.advanceTimersByTime(100);
            expect(events).toHaveLength(0);
        });
    });

    describe("info イベントの内容", () => {
        it("info イベントに正しいデータが含まれる", async () => {
            const client = createMockEngineClient();
            const infoEvents: EngineInfoEvent[] = [];

            client.subscribe((event) => {
                if (event.type === "info") {
                    infoEvents.push(event);
                }
            });

            await client.search({});
            vi.advanceTimersByTime(10);

            expect(infoEvents).toHaveLength(1);

            const infoEvent = infoEvents[0];
            expect(infoEvent.type).toBe("info");
            expect(infoEvent.depth).toBe(1);
            expect(infoEvent.scoreCp).toBe(0);
            expect(infoEvent.nodes).toBe(128);
            expect(infoEvent.nps).toBe(1024);
            expect(infoEvent.pv).toEqual([]);
        });
    });
});

describe("validateSkillLevelSettings", () => {
    it("有効な設定で valid: true を返す", () => {
        const result = validateSkillLevelSettings({ skillLevel: 10 });
        expect(result.valid).toBe(true);
        expect(result.errors).toHaveLength(0);
    });

    it("skillLevel が最小値の場合に有効", () => {
        const result = validateSkillLevelSettings({ skillLevel: SKILL_LEVEL_MIN });
        expect(result.valid).toBe(true);
    });

    it("skillLevel が最大値の場合に有効", () => {
        const result = validateSkillLevelSettings({ skillLevel: SKILL_LEVEL_MAX });
        expect(result.valid).toBe(true);
    });

    it("skillLevel が範囲外（負数）の場合にエラー", () => {
        const result = validateSkillLevelSettings({ skillLevel: -1 });
        expect(result.valid).toBe(false);
        expect(result.errors).toHaveLength(1);
        expect(result.errors[0]).toContain("skillLevel");
    });

    it("skillLevel が範囲外（上限超え）の場合にエラー", () => {
        const result = validateSkillLevelSettings({ skillLevel: 21 });
        expect(result.valid).toBe(false);
        expect(result.errors).toHaveLength(1);
    });

    it("useLimitStrength=true で有効な ELO の場合に有効", () => {
        const result = validateSkillLevelSettings({
            skillLevel: 10,
            useLimitStrength: true,
            elo: 2000,
        });
        expect(result.valid).toBe(true);
    });

    it("useLimitStrength=true で ELO が範囲外の場合にエラー", () => {
        const result = validateSkillLevelSettings({
            skillLevel: 10,
            useLimitStrength: true,
            elo: 1000, // ELO_MIN = 1320 より小さい
        });
        expect(result.valid).toBe(false);
        expect(result.errors[0]).toContain("elo");
    });

    it("useLimitStrength=true で ELO が上限超えの場合にエラー", () => {
        const result = validateSkillLevelSettings({
            skillLevel: 10,
            useLimitStrength: true,
            elo: 4000, // ELO_MAX = 3190 より大きい
        });
        expect(result.valid).toBe(false);
    });

    it("useLimitStrength=false の場合は ELO のバリデーションをスキップ", () => {
        const result = validateSkillLevelSettings({
            skillLevel: 10,
            useLimitStrength: false,
            elo: 1000, // 範囲外だが無視される
        });
        expect(result.valid).toBe(true);
    });
});

describe("normalizeSkillLevelSettings", () => {
    it("有効な設定はそのまま返す", () => {
        const input: SkillLevelSettings = { skillLevel: 10 };
        const result = normalizeSkillLevelSettings(input);
        expect(result.skillLevel).toBe(10);
    });

    it("skillLevel が負数の場合は最小値にクランプ", () => {
        const result = normalizeSkillLevelSettings({ skillLevel: -5 });
        expect(result.skillLevel).toBe(SKILL_LEVEL_MIN);
    });

    it("skillLevel が上限超えの場合は最大値にクランプ", () => {
        const result = normalizeSkillLevelSettings({ skillLevel: 100 });
        expect(result.skillLevel).toBe(SKILL_LEVEL_MAX);
    });

    it("useLimitStrength=true の場合は ELO もクランプ", () => {
        const result = normalizeSkillLevelSettings({
            skillLevel: 10,
            useLimitStrength: true,
            elo: 500,
        });
        expect(result.skillLevel).toBe(10);
        expect(result.useLimitStrength).toBe(true);
        expect(result.elo).toBe(ELO_MIN);
    });

    it("ELO が上限超えの場合は最大値にクランプ", () => {
        const result = normalizeSkillLevelSettings({
            skillLevel: 10,
            useLimitStrength: true,
            elo: 5000,
        });
        expect(result.elo).toBe(ELO_MAX);
    });

    it("useLimitStrength=false の場合は ELO 関連のプロパティを含めない", () => {
        const result = normalizeSkillLevelSettings({
            skillLevel: 10,
            useLimitStrength: false,
            elo: 2000,
        });
        expect(result).toEqual({ skillLevel: 10 });
        expect(result.useLimitStrength).toBeUndefined();
        expect(result.elo).toBeUndefined();
    });
});

describe("detectSkillPreset", () => {
    it("skillLevel=2 は beginner プリセットを返す", () => {
        const result = detectSkillPreset({ skillLevel: 2 });
        expect(result).toBe("beginner");
    });

    it("skillLevel=10 は intermediate プリセットを返す", () => {
        const result = detectSkillPreset({ skillLevel: 10 });
        expect(result).toBe("intermediate");
    });

    it("skillLevel=16 は advanced プリセットを返す", () => {
        const result = detectSkillPreset({ skillLevel: 16 });
        expect(result).toBe("advanced");
    });

    it("skillLevel=20 は professional プリセットを返す", () => {
        const result = detectSkillPreset({ skillLevel: 20 });
        expect(result).toBe("professional");
    });

    it("プリセットに一致しない skillLevel は custom を返す", () => {
        const result = detectSkillPreset({ skillLevel: 5 });
        expect(result).toBe("custom");
    });

    it("useLimitStrength=true の場合は常に custom を返す", () => {
        const result = detectSkillPreset({
            skillLevel: 10, // intermediate に一致するが
            useLimitStrength: true,
        });
        expect(result).toBe("custom");
    });

    it("範囲外の skillLevel は custom を返す", () => {
        expect(detectSkillPreset({ skillLevel: -1 })).toBe("custom");
        expect(detectSkillPreset({ skillLevel: 21 })).toBe("custom");
    });
});
