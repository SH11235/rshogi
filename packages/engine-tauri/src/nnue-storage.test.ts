import type { NnueMeta } from "@shogi/app-core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createTauriNnueStorage, type TauriNnueStorageOptions } from "./nnue-storage";

type BufferLike = {
    from: (data: string, encoding?: "binary") => { toString: (encoding: "base64") => string };
};

const fallbackBtoa = (data: string): string => {
    const bufferCtor = (globalThis as { Buffer?: BufferLike }).Buffer;
    if (!bufferCtor) {
        throw new Error("Buffer is not available for base64 encoding");
    }
    return bufferCtor.from(data, "binary").toString("base64");
};

const encodeBase64 = (data: string): string => (globalThis.btoa ?? fallbackBtoa)(data);

const bytesToBase64 = (bytes: Uint8Array): string => {
    let binary = "";
    for (const byte of bytes) {
        binary += String.fromCharCode(byte);
    }
    return encodeBase64(binary);
};

function createLocalStorageMock(): Storage {
    const store = new Map<string, string>();
    return {
        getItem: (key: string) => store.get(key) ?? null,
        setItem: (key: string, value: string) => {
            store.set(key, value);
        },
        removeItem: (key: string) => {
            store.delete(key);
        },
        clear: () => {
            store.clear();
        },
        key: (index: number) => Array.from(store.keys())[index] ?? null,
        get length() {
            return store.size;
        },
    };
}

describe("nnue-storage", () => {
    let originalLocalStorage: Storage | undefined;
    let originalBtoa: typeof btoa | undefined;

    beforeEach(() => {
        originalLocalStorage = globalThis.localStorage;
        originalBtoa = globalThis.btoa;
        globalThis.localStorage = createLocalStorageMock();
        if (!globalThis.btoa) {
            globalThis.btoa = fallbackBtoa;
        }
    });

    afterEach(() => {
        if (originalLocalStorage) {
            globalThis.localStorage = originalLocalStorage;
        } else {
            delete (globalThis as { localStorage?: Storage }).localStorage;
        }
        if (originalBtoa) {
            globalThis.btoa = originalBtoa;
        } else {
            delete (globalThis as { btoa?: typeof btoa }).btoa;
        }
        vi.clearAllMocks();
    });

    it("encodes binary chunks into base64 without loss", async () => {
        const bytes = new Uint8Array([0x00, 0x41, 0x80, 0x9f, 0xff]);
        const id = "test-id";
        const meta: NnueMeta = {
            id,
            displayName: "test",
            originalFileName: "test.nnue",
            size: bytes.length,
            contentHashSha256: "dummy-hash",
            source: "user-uploaded",
            createdAt: 0,
            verified: false,
        };

        const mockInvoke = vi.fn().mockResolvedValue(undefined);
        const storage = createTauriNnueStorage({
            invoke: mockInvoke as TauriNnueStorageOptions["invoke"],
        });

        await storage.save(id, bytes, meta);

        const saveCall = mockInvoke.mock.calls.find(([command]) => command === "save_nnue_chunk");
        expect(saveCall).toBeDefined();

        if (!saveCall) {
            throw new Error("save_nnue_chunk was not called");
        }

        const payload = saveCall[1] as { dataBase64: string; chunkIndex: number; id: string };
        expect(payload.id).toBe(id);
        expect(payload.chunkIndex).toBe(0);

        const expectedBase64 = bytesToBase64(bytes);
        expect(payload.dataBase64).toBe(expectedBase64);
    });
});
