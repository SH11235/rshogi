import type { NnueStorage } from "@shogi/app-core";
import { renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import { NnueProvider } from "../providers/NnueContext";
import { useNnueStorage } from "./useNnueStorage";

// Web 用モックストレージ（supportsFileImport=true, supportsLoad=true）
const createWebMockStorage = (): NnueStorage => ({
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
});

// Desktop 用モックストレージ（supportsPathImport=true, supportsLoad=false）
const createDesktopMockStorage = (): NnueStorage => ({
    capabilities: {
        supportsFileImport: false,
        supportsPathImport: true,
        supportsLoad: false,
    },
    save: vi.fn().mockResolvedValue(undefined),
    // load/loadStream は存在しない
    delete: vi.fn().mockResolvedValue(undefined),
    listMeta: vi.fn().mockResolvedValue([]),
    getMeta: vi.fn().mockResolvedValue(null),
    updateMeta: vi.fn().mockResolvedValue(undefined),
    getUsage: vi.fn().mockResolvedValue({ used: 0 }),
    listByContentHash: vi.fn().mockResolvedValue([]),
    listByPresetKey: vi.fn().mockResolvedValue([]),
    importFromPath: vi.fn().mockResolvedValue({
        id: "test-id",
        displayName: "Test",
        originalFileName: "test.nnue",
        size: 1024,
        contentHashSha256: "abc",
        source: "user-uploaded",
        createdAt: Date.now(),
        verified: false,
    }),
});

// NnueProvider でラップする wrapper
const createWrapper = (storage: NnueStorage) => {
    return function Wrapper({ children }: { children: ReactNode }) {
        return <NnueProvider storage={storage}>{children}</NnueProvider>;
    };
};

describe("useNnueStorage", () => {
    describe("Web storage (supportsFileImport=true, supportsLoad=true)", () => {
        it("capabilities が正しく返される", () => {
            const storage = createWebMockStorage();
            const { result } = renderHook(() => useNnueStorage(), {
                wrapper: createWrapper(storage),
            });

            expect(result.current.capabilities).toEqual({
                supportsFileImport: true,
                supportsPathImport: false,
                supportsLoad: true,
            });
        });
    });

    describe("Desktop storage (supportsPathImport=true, supportsLoad=false)", () => {
        it("capabilities が正しく返される", () => {
            const storage = createDesktopMockStorage();
            const { result } = renderHook(() => useNnueStorage(), {
                wrapper: createWrapper(storage),
            });

            expect(result.current.capabilities).toEqual({
                supportsFileImport: false,
                supportsPathImport: true,
                supportsLoad: false,
            });
        });
    });

    describe("NnueProvider 外での使用", () => {
        it("capabilities が null を返す", () => {
            const { result } = renderHook(() => useNnueStorage());

            expect(result.current.capabilities).toBeNull();
        });
    });
});
