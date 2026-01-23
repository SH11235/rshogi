import type { NnueMeta, NnueStorage, PresetManifest } from "@shogi/app-core";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NnueProvider } from "../providers/NnueContext";
import { getDownloadedMeta, usePresetManager } from "./usePresetManager";

// モック fetch（各テスト後に復元）
const mockFetch = vi.fn();

beforeEach(() => {
    vi.stubGlobal("fetch", mockFetch);
});

afterEach(() => {
    vi.unstubAllGlobals();
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

// テスト用の manifest
const createTestManifest = (): PresetManifest => ({
    version: 1,
    updatedAt: "2024-01-01T00:00:00Z",
    presets: [
        {
            presetKey: "test-nnue",
            displayName: "Test NNUE",
            description: "Test description",
            url: "https://example.com/test.nnue",
            size: 1024,
            sha256: "abc123def456abc123def456abc123def456abc123def456abc123def456abc1",
            license: "MIT",
            releasedAt: "2024-01-01",
        },
    ],
});

// モックストレージを作成
const createMockStorage = (overrides: Partial<NnueStorage> = {}): NnueStorage => ({
    capabilities: {
        supportsFileImport: true,
        supportsPathImport: false,
        supportsLoad: true,
    },
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

// NnueProvider でラップする wrapper
const createWrapper = (storage: NnueStorage) => {
    return function Wrapper({ children }: { children: ReactNode }) {
        return <NnueProvider storage={storage}>{children}</NnueProvider>;
    };
};

describe("usePresetManager", () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    afterEach(() => {
        vi.restoreAllMocks();
    });

    describe("初期状態", () => {
        it("NnueProvider 外では isConfigured が false を返す", () => {
            const { result } = renderHook(() =>
                usePresetManager({ manifestUrl: "https://example.com/manifest.json" }),
            );

            expect(result.current.isConfigured).toBe(false);
            expect(result.current.presets).toEqual([]);
        });

        it("manifestUrl がない場合は isConfigured が false を返す", () => {
            const storage = createMockStorage();
            const { result } = renderHook(() => usePresetManager(), {
                wrapper: createWrapper(storage),
            });

            expect(result.current.isConfigured).toBe(false);
        });

        it("正しく設定されている場合は isConfigured が true を返す", () => {
            const storage = createMockStorage();
            const { result } = renderHook(
                () => usePresetManager({ manifestUrl: "https://example.com/manifest.json" }),
                { wrapper: createWrapper(storage) },
            );

            expect(result.current.isConfigured).toBe(true);
        });
    });

    describe("自動フェッチ", () => {
        it("autoFetch=true の場合、マウント時にプリセット一覧を取得する", async () => {
            const manifest = createTestManifest();
            const storage = createMockStorage();

            mockFetch.mockResolvedValue({
                ok: true,
                json: () => Promise.resolve(manifest),
            });

            const { result } = renderHook(
                () =>
                    usePresetManager({
                        manifestUrl: "https://example.com/manifest.json",
                        autoFetch: true,
                    }),
                { wrapper: createWrapper(storage) },
            );

            expect(result.current.isLoading).toBe(true);

            await waitFor(() => {
                expect(result.current.isLoading).toBe(false);
            });

            expect(result.current.presets).toHaveLength(1);
            expect(result.current.presets[0].config.presetKey).toBe("test-nnue");
        });

        it("autoFetch=false の場合、マウント時にフェッチしない", async () => {
            const storage = createMockStorage();

            const { result } = renderHook(
                () =>
                    usePresetManager({
                        manifestUrl: "https://example.com/manifest.json",
                        autoFetch: false,
                    }),
                { wrapper: createWrapper(storage) },
            );

            // フェッチが呼ばれないことを確認
            expect(mockFetch).not.toHaveBeenCalled();
            expect(result.current.presets).toEqual([]);
        });
    });

    describe("refresh", () => {
        it("refresh でプリセット一覧を再取得できる", async () => {
            const manifest = createTestManifest();
            const storage = createMockStorage();

            mockFetch.mockResolvedValue({
                ok: true,
                json: () => Promise.resolve(manifest),
            });

            const { result } = renderHook(
                () =>
                    usePresetManager({
                        manifestUrl: "https://example.com/manifest.json",
                        autoFetch: false,
                    }),
                { wrapper: createWrapper(storage) },
            );

            expect(result.current.presets).toEqual([]);

            await act(async () => {
                await result.current.refresh();
            });

            expect(result.current.presets).toHaveLength(1);
        });

        it("refresh 失敗時にエラーを設定する", async () => {
            const storage = createMockStorage();

            mockFetch.mockRejectedValue(new Error("Network error"));

            const { result } = renderHook(
                () =>
                    usePresetManager({
                        manifestUrl: "https://example.com/manifest.json",
                        autoFetch: false,
                    }),
                { wrapper: createWrapper(storage) },
            );

            await act(async () => {
                await result.current.refresh();
            });

            expect(result.current.error).not.toBeNull();
            expect(result.current.error?.code).toBe("NNUE_DOWNLOAD_FAILED");
        });
    });

    describe("download", () => {
        it("manager が初期化されていない場合はエラーを返す", async () => {
            const { result } = renderHook(() => usePresetManager());

            await act(async () => {
                await result.current.download("test-nnue");
            });

            expect(result.current.error).not.toBeNull();
            expect(result.current.error?.code).toBe("NNUE_STORAGE_FAILED");
        });

        it("既にダウンロード中の場合はエラーを返す", async () => {
            const manifest = createTestManifest();
            const storage = createMockStorage();

            // ダウンロードが完了しないモック
            let resolveDownload: () => void;
            mockFetch
                .mockResolvedValueOnce({
                    ok: true,
                    json: () => Promise.resolve(manifest),
                })
                .mockImplementationOnce(
                    () =>
                        new Promise((resolve) => {
                            resolveDownload = () =>
                                resolve({
                                    ok: true,
                                    headers: new Headers(),
                                    body: new ReadableStream({
                                        start(controller) {
                                            controller.close();
                                        },
                                    }),
                                });
                        }),
                );

            const { result } = renderHook(
                () =>
                    usePresetManager({
                        manifestUrl: "https://example.com/manifest.json",
                        autoFetch: false,
                    }),
                { wrapper: createWrapper(storage) },
            );

            // 1つ目のダウンロードを開始（完了しない）
            act(() => {
                void result.current.download("test-nnue");
            });

            // downloadingKey が設定されるのを待つ
            await waitFor(() => {
                expect(result.current.downloadingKey).toBe("test-nnue");
            });

            // 2つ目のダウンロードを試みる
            await act(async () => {
                await result.current.download("test-nnue");
            });

            expect(result.current.error?.code).toBe("NNUE_DOWNLOAD_IN_PROGRESS");

            // クリーンアップ
            resolveDownload!();
        });

        it("重複ファイルがある場合は既存のメタを返す", async () => {
            const manifest = createTestManifest();
            const existingMeta = createTestMeta();
            const storage = createMockStorage({
                listByContentHash: vi.fn().mockResolvedValue([existingMeta]),
            });

            mockFetch.mockResolvedValue({
                ok: true,
                json: () => Promise.resolve(manifest),
            });

            const onDownloadComplete = vi.fn();

            const { result } = renderHook(
                () =>
                    usePresetManager({
                        manifestUrl: "https://example.com/manifest.json",
                        autoFetch: false,
                        onDownloadComplete,
                    }),
                { wrapper: createWrapper(storage) },
            );

            let downloadResult: NnueMeta | undefined;
            await act(async () => {
                downloadResult = await result.current.download("test-nnue");
            });

            expect(downloadResult).toEqual(existingMeta);
            expect(onDownloadComplete).toHaveBeenCalledWith(existingMeta);
        });
    });

    describe("clearError", () => {
        it("エラーをクリアできる", async () => {
            const storage = createMockStorage();

            mockFetch.mockRejectedValue(new Error("Network error"));

            const { result } = renderHook(
                () =>
                    usePresetManager({
                        manifestUrl: "https://example.com/manifest.json",
                        autoFetch: false,
                    }),
                { wrapper: createWrapper(storage) },
            );

            await act(async () => {
                await result.current.refresh();
            });

            expect(result.current.error).not.toBeNull();

            act(() => {
                result.current.clearError();
            });

            expect(result.current.error).toBeNull();
        });
    });
});

describe("getDownloadedMeta", () => {
    it("localMetas が空の場合は null を返す", () => {
        const result = getDownloadedMeta({
            config: {
                presetKey: "test",
                displayName: "Test",
                description: "Test",
                url: "https://example.com/test.nnue",
                size: 1024,
                sha256: "hash123",
                license: "MIT",
                releasedAt: "2024-01-01",
            },
            status: "not-downloaded",
            localMetas: [],
        });

        expect(result.meta).toBeNull();
        expect(result.isLatest).toBe(false);
    });

    it("最新のハッシュと一致するメタがある場合はそれを返す", () => {
        const latestMeta = createTestMeta({ contentHashSha256: "hash123", createdAt: 1000 });
        const oldMeta = createTestMeta({
            id: "old",
            contentHashSha256: "old-hash",
            createdAt: 2000,
        });

        const result = getDownloadedMeta({
            config: {
                presetKey: "test",
                displayName: "Test",
                description: "Test",
                url: "https://example.com/test.nnue",
                size: 1024,
                sha256: "hash123",
                license: "MIT",
                releasedAt: "2024-01-01",
            },
            status: "latest",
            localMetas: [oldMeta, latestMeta],
        });

        expect(result.meta).toEqual(latestMeta);
        expect(result.isLatest).toBe(true);
    });

    it("最新のハッシュと一致しない場合は最新の作成日時のものを返す", () => {
        const oldMeta = createTestMeta({
            id: "old",
            contentHashSha256: "old-hash",
            createdAt: 1000,
        });
        const newerMeta = createTestMeta({
            id: "newer",
            contentHashSha256: "newer-hash",
            createdAt: 2000,
        });

        const result = getDownloadedMeta({
            config: {
                presetKey: "test",
                displayName: "Test",
                description: "Test",
                url: "https://example.com/test.nnue",
                size: 1024,
                sha256: "different-hash", // どちらとも一致しない
                license: "MIT",
                releasedAt: "2024-01-01",
            },
            status: "update-available",
            localMetas: [oldMeta, newerMeta],
        });

        expect(result.meta).toEqual(newerMeta);
        expect(result.isLatest).toBe(false);
    });
});
