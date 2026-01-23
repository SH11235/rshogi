import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NnueError } from "./errors";
import {
    createPresetManager,
    downloadPreset,
    fetchPresetManifest,
    getAllPresetStatuses,
    getPresetStatus,
} from "./preset-manager";
import type { NnueStorage } from "./storage";
import type { NnueDownloadProgress, NnueMeta, PresetConfig, PresetManifest } from "./types";

// モック fetch（各テスト後に復元）
const mockFetch = vi.fn();
let originalFetch: typeof fetch;

// テスト用のプリセット設定
const createTestPreset = (overrides: Partial<PresetConfig> = {}): PresetConfig => ({
    presetKey: "test-nnue",
    displayName: "Test NNUE",
    description: "Test description",
    url: "https://example.com/test.nnue",
    size: 1024,
    sha256: "abc123def456abc123def456abc123def456abc123def456abc123def456abc1",
    license: "MIT",
    releasedAt: "2024-01-01",
    ...overrides,
});

// テスト用の manifest
const createTestManifest = (presets: PresetConfig[] = [createTestPreset()]): PresetManifest => ({
    version: 1,
    updatedAt: "2024-01-01T00:00:00Z",
    presets,
});

// テスト用の NnueMeta
const createTestMeta = (overrides: Partial<NnueMeta> = {}): NnueMeta => ({
    id: "meta-id-123",
    displayName: "Test NNUE",
    originalFileName: "test.nnue",
    size: 1024,
    contentHashSha256: "abc123def456abc123def456abc123def456abc123def456abc123def456abc1",
    source: "preset",
    presetKey: "test-nnue",
    createdAt: Date.now(),
    verified: false,
    ...overrides,
});

// モックストレージを作成
const createMockStorage = (overrides: Partial<NnueStorage> = {}): NnueStorage => ({
    save: vi.fn().mockResolvedValue(undefined),
    load: vi.fn().mockResolvedValue(new Uint8Array()),
    loadStream: vi.fn().mockResolvedValue(new ReadableStream()),
    delete: vi.fn().mockResolvedValue(undefined),
    listMeta: vi.fn().mockResolvedValue([]),
    getMeta: vi.fn().mockResolvedValue(null),
    updateMeta: vi.fn().mockResolvedValue(undefined),
    getUsage: vi.fn().mockResolvedValue({ used: 0 }),
    listByContentHash: vi.fn().mockResolvedValue([]),
    listByPresetKey: vi.fn().mockResolvedValue([]),
    ...overrides,
});

// fetch モックのセットアップと復元
beforeEach(() => {
    originalFetch = global.fetch;
    global.fetch = mockFetch;
});

afterEach(() => {
    global.fetch = originalFetch;
});

// ReadableStream をモック
function createMockReadableStream(data: Uint8Array): ReadableStream<Uint8Array> {
    let sent = false;
    return new ReadableStream({
        pull(controller) {
            if (!sent) {
                controller.enqueue(data);
                sent = true;
            } else {
                controller.close();
            }
        },
    });
}

describe("fetchPresetManifest", () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it("manifest.json を正常に取得できる", async () => {
        const manifest = createTestManifest();
        mockFetch.mockResolvedValueOnce({
            ok: true,
            json: () => Promise.resolve(manifest),
        });

        const result = await fetchPresetManifest("https://example.com/manifest.json");

        expect(result).toEqual(manifest);
        expect(mockFetch).toHaveBeenCalledWith("https://example.com/manifest.json");
    });

    it("HTTP エラーで NnueError をスローする", async () => {
        mockFetch.mockResolvedValueOnce({
            ok: false,
            status: 404,
        });

        await expect(fetchPresetManifest("https://example.com/manifest.json")).rejects.toThrow(
            NnueError,
        );
        await expect(
            fetchPresetManifest("https://example.com/manifest.json"),
        ).rejects.toMatchObject({
            code: "NNUE_DOWNLOAD_FAILED",
        });
    });

    it("ネットワークエラーで NnueError をスローする", async () => {
        mockFetch.mockRejectedValueOnce(new Error("Network error"));

        await expect(fetchPresetManifest("https://example.com/manifest.json")).rejects.toThrow(
            NnueError,
        );
    });
});

describe("downloadPreset", () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    afterEach(() => {
        vi.restoreAllMocks();
    });

    it("サイズ超過で NNUE_SIZE_EXCEEDED エラーをスローする", async () => {
        const preset = createTestPreset({ size: 500 * 1024 * 1024 }); // 500MB
        const storage = createMockStorage();

        await expect(downloadPreset(preset, storage)).rejects.toMatchObject({
            code: "NNUE_SIZE_EXCEEDED",
        });
    });

    it("ダウンロード失敗で NNUE_DOWNLOAD_FAILED エラーをスローする", async () => {
        const preset = createTestPreset();
        const storage = createMockStorage();

        mockFetch.mockResolvedValueOnce({
            ok: false,
            status: 500,
        });

        await expect(downloadPreset(preset, storage)).rejects.toMatchObject({
            code: "NNUE_DOWNLOAD_FAILED",
        });
    });

    it("Content-Length 不一致（Content-Encoding なし）で NNUE_SIZE_MISMATCH エラーをスローする", async () => {
        const preset = createTestPreset({ size: 1024 });
        const storage = createMockStorage();

        mockFetch.mockResolvedValueOnce({
            ok: true,
            headers: new Headers({
                "Content-Length": "2048", // 期待と異なるサイズ
            }),
            body: createMockReadableStream(new Uint8Array(1024)),
        });

        await expect(downloadPreset(preset, storage)).rejects.toMatchObject({
            code: "NNUE_SIZE_MISMATCH",
        });
    });

    it("Content-Encoding がある場合は Content-Length チェックをスキップする", async () => {
        const data = new Uint8Array(1024);
        const preset = createTestPreset({ size: 1024, sha256: "wrong-hash" });
        const storage = createMockStorage();

        mockFetch.mockResolvedValueOnce({
            ok: true,
            headers: new Headers({
                "Content-Length": "512", // 圧縮後のサイズ（実際とは異なる）
                "Content-Encoding": "gzip",
            }),
            body: createMockReadableStream(data),
        });

        // Content-Length チェックはスキップされるが、ハッシュ不一致でエラー
        await expect(downloadPreset(preset, storage)).rejects.toMatchObject({
            code: "NNUE_HASH_MISMATCH",
        });
    });

    it("ダウンロード後のサイズ不一致で NNUE_SIZE_MISMATCH エラーをスローする", async () => {
        const preset = createTestPreset({ size: 1024 });
        const storage = createMockStorage();

        // 実際には 512 バイトしかダウンロードされない
        mockFetch.mockResolvedValueOnce({
            ok: true,
            headers: new Headers(),
            body: createMockReadableStream(new Uint8Array(512)),
        });

        await expect(downloadPreset(preset, storage)).rejects.toMatchObject({
            code: "NNUE_SIZE_MISMATCH",
        });
    });

    it("ハッシュ不一致で NNUE_HASH_MISMATCH エラーをスローする", async () => {
        const data = new Uint8Array(1024);
        const preset = createTestPreset({ size: 1024, sha256: "wrong-hash" });
        const storage = createMockStorage();

        mockFetch.mockResolvedValueOnce({
            ok: true,
            headers: new Headers(),
            body: createMockReadableStream(data),
        });

        await expect(downloadPreset(preset, storage)).rejects.toMatchObject({
            code: "NNUE_HASH_MISMATCH",
        });
    });

    it("正常にダウンロードして保存できる", async () => {
        // 正しいハッシュを生成するためのデータ
        const data = new Uint8Array([1, 2, 3, 4]);
        // crypto.subtle.digest で計算したハッシュ
        const hashBuffer = await crypto.subtle.digest("SHA-256", data.buffer);
        const hashArray = Array.from(new Uint8Array(hashBuffer));
        const hash = hashArray.map((b) => b.toString(16).padStart(2, "0")).join("");

        const preset = createTestPreset({ size: 4, sha256: hash });
        const storage = createMockStorage();

        mockFetch.mockResolvedValueOnce({
            ok: true,
            headers: new Headers(),
            body: createMockReadableStream(data),
        });

        const result = await downloadPreset(preset, storage);

        expect(result).toMatchObject({
            displayName: preset.displayName,
            size: 4,
            contentHashSha256: hash,
            source: "preset",
            presetKey: preset.presetKey,
        });
        expect(storage.save).toHaveBeenCalledTimes(1);
    });

    it("進捗ハンドラーが呼ばれる", async () => {
        const data = new Uint8Array([1, 2, 3, 4]);
        const hashBuffer = await crypto.subtle.digest("SHA-256", data.buffer);
        const hashArray = Array.from(new Uint8Array(hashBuffer));
        const hash = hashArray.map((b) => b.toString(16).padStart(2, "0")).join("");

        const preset = createTestPreset({ size: 4, sha256: hash });
        const storage = createMockStorage();
        const progressHandler = vi.fn();

        mockFetch.mockResolvedValueOnce({
            ok: true,
            headers: new Headers(),
            body: createMockReadableStream(data),
        });

        await downloadPreset(preset, storage, progressHandler);

        // 進捗ハンドラーが呼ばれることを確認
        expect(progressHandler).toHaveBeenCalled();

        // downloading フェーズが呼ばれる
        const downloadingCalls = progressHandler.mock.calls.filter(
            (call: [NnueDownloadProgress]) => call[0].phase === "downloading",
        );
        expect(downloadingCalls.length).toBeGreaterThanOrEqual(1);

        // validating フェーズが呼ばれる
        const validatingCalls = progressHandler.mock.calls.filter(
            (call: [NnueDownloadProgress]) => call[0].phase === "validating",
        );
        expect(validatingCalls.length).toBe(1);

        // saving フェーズが呼ばれる
        const savingCalls = progressHandler.mock.calls.filter(
            (call: [NnueDownloadProgress]) => call[0].phase === "saving",
        );
        expect(savingCalls.length).toBe(1);
    });
});

describe("getPresetStatus", () => {
    it("ローカルに存在しない場合は not-downloaded を返す", async () => {
        const preset = createTestPreset();
        const storage = createMockStorage({
            listByPresetKey: vi.fn().mockResolvedValue([]),
        });

        const result = await getPresetStatus(preset, storage);

        expect(result.status).toBe("not-downloaded");
        expect(result.localMetas).toEqual([]);
    });

    it("SHA-256 が一致する場合は latest を返す", async () => {
        const preset = createTestPreset();
        const meta = createTestMeta({ contentHashSha256: preset.sha256 });
        const storage = createMockStorage({
            listByPresetKey: vi.fn().mockResolvedValue([meta]),
        });

        const result = await getPresetStatus(preset, storage);

        expect(result.status).toBe("latest");
        expect(result.localMetas).toEqual([meta]);
    });

    it("SHA-256 が異なる場合は update-available を返す", async () => {
        const preset = createTestPreset();
        const meta = createTestMeta({ contentHashSha256: "different-hash" });
        const storage = createMockStorage({
            listByPresetKey: vi.fn().mockResolvedValue([meta]),
        });

        const result = await getPresetStatus(preset, storage);

        expect(result.status).toBe("update-available");
        expect(result.localMetas).toEqual([meta]);
    });
});

describe("getAllPresetStatuses", () => {
    it("全プリセットの状態を取得できる", async () => {
        const preset1 = createTestPreset({ presetKey: "preset1" });
        const preset2 = createTestPreset({ presetKey: "preset2", sha256: "different-hash" });
        const manifest = createTestManifest([preset1, preset2]);

        const meta1 = createTestMeta({ presetKey: "preset1", contentHashSha256: preset1.sha256 });

        const storage = createMockStorage({
            listByPresetKey: vi.fn().mockImplementation((key: string) => {
                if (key === "preset1") return Promise.resolve([meta1]);
                return Promise.resolve([]);
            }),
        });

        const results = await getAllPresetStatuses(manifest, storage);

        expect(results).toHaveLength(2);
        expect(results[0].config.presetKey).toBe("preset1");
        expect(results[0].status).toBe("latest");
        expect(results[1].config.presetKey).toBe("preset2");
        expect(results[1].status).toBe("not-downloaded");
    });
});

describe("createPresetManager", () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it("manifest をキャッシュする", async () => {
        const manifest = createTestManifest();
        const storage = createMockStorage();

        mockFetch.mockResolvedValue({
            ok: true,
            json: () => Promise.resolve(manifest),
        });

        const manager = createPresetManager({
            manifestUrl: "https://example.com/manifest.json",
            storage,
        });

        // 1回目
        await manager.getManifest();
        expect(mockFetch).toHaveBeenCalledTimes(1);

        // 2回目（キャッシュから）
        await manager.getManifest();
        expect(mockFetch).toHaveBeenCalledTimes(1);

        // 強制リフレッシュ
        await manager.getManifest(true);
        expect(mockFetch).toHaveBeenCalledTimes(2);
    });

    it("clearCache で manifest キャッシュをクリアできる", async () => {
        const manifest = createTestManifest();
        const storage = createMockStorage();

        mockFetch.mockResolvedValue({
            ok: true,
            json: () => Promise.resolve(manifest),
        });

        const manager = createPresetManager({
            manifestUrl: "https://example.com/manifest.json",
            storage,
        });

        await manager.getManifest();
        expect(mockFetch).toHaveBeenCalledTimes(1);

        manager.clearCache();

        await manager.getManifest();
        expect(mockFetch).toHaveBeenCalledTimes(2);
    });

    it("isDuplicate で重複をチェックできる", async () => {
        const preset = createTestPreset();
        const manifest = createTestManifest([preset]);
        const meta = createTestMeta({ contentHashSha256: preset.sha256 });

        const storage = createMockStorage({
            listByContentHash: vi.fn().mockResolvedValue([meta]),
        });

        mockFetch.mockResolvedValue({
            ok: true,
            json: () => Promise.resolve(manifest),
        });

        const manager = createPresetManager({
            manifestUrl: "https://example.com/manifest.json",
            storage,
        });

        const result = await manager.isDuplicate("test-nnue");

        expect(result).toBe(true);
        expect(storage.listByContentHash).toHaveBeenCalledWith(preset.sha256);
    });

    it("存在しないプリセットの download で NNUE_NOT_FOUND エラーをスローする", async () => {
        const manifest = createTestManifest([]);
        const storage = createMockStorage();

        mockFetch.mockResolvedValue({
            ok: true,
            json: () => Promise.resolve(manifest),
        });

        const manager = createPresetManager({
            manifestUrl: "https://example.com/manifest.json",
            storage,
        });

        await expect(manager.download("non-existent")).rejects.toMatchObject({
            code: "NNUE_NOT_FOUND",
        });
    });
});
