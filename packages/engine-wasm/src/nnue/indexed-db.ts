/**
 * IndexedDB を使った NNUE ストレージ実装
 */

import {
    NNUE_DB_NAME,
    NNUE_DB_VERSION,
    NNUE_MAX_SIZE_BYTES,
    NnueError,
    type NnueMeta,
    type NnueStorage,
} from "@shogi/app-core";
import { type DBSchema, type IDBPDatabase, openDB } from "idb";

/**
 * IndexedDB スキーマ定義
 */
interface NnueDBSchema extends DBSchema {
    "nnue-blobs": {
        key: string;
        value: Blob;
    };
    "nnue-meta": {
        key: string;
        value: NnueMeta;
        indexes: {
            "by-source": string;
            "by-created": number;
            "by-preset-key": string;
            "by-content-hash": string;
        };
    };
}

let dbPromise: Promise<IDBPDatabase<NnueDBSchema>> | null = null;

/**
 * IndexedDB を開く
 */
function getNnueDB(): Promise<IDBPDatabase<NnueDBSchema>> {
    if (!dbPromise) {
        dbPromise = openDB<NnueDBSchema>(NNUE_DB_NAME, NNUE_DB_VERSION, {
            upgrade(db) {
                // nnue-blobs store（キーは外部指定）
                if (!db.objectStoreNames.contains("nnue-blobs")) {
                    db.createObjectStore("nnue-blobs");
                }

                // nnue-meta store（keyPath で id を自動キーに）
                if (!db.objectStoreNames.contains("nnue-meta")) {
                    const metaStore = db.createObjectStore("nnue-meta", { keyPath: "id" });
                    metaStore.createIndex("by-source", "source");
                    metaStore.createIndex("by-created", "createdAt");
                    metaStore.createIndex("by-preset-key", "presetKey");
                    metaStore.createIndex("by-content-hash", "contentHashSha256");
                }
            },
        });
    }
    return dbPromise;
}

/**
 * IndexedDB エラーを NnueError にマッピング
 */
function mapIdbError(error: unknown): NnueError {
    if (error instanceof DOMException) {
        if (error.name === "QuotaExceededError") {
            return new NnueError("NNUE_STORAGE_FULL", "ストレージ容量が不足しています", error);
        }
    }
    return new NnueError("NNUE_STORAGE_FAILED", "保存に失敗しました", error);
}

/**
 * IndexedDB ベースの NnueStorage 実装
 */
export function createIndexedDBNnueStorage(): NnueStorage {
    return {
        capabilities: {
            supportsFileImport: true, // drag&drop + input[type=file]
            supportsPathImport: false, // Tauri 専用
            supportsLoad: true, // IndexedDB から読み込み可能
        },

        async save(id: string, data: Blob | Uint8Array, meta: NnueMeta): Promise<void> {
            // ID の一致を確認
            if (meta.id !== id) {
                throw new NnueError(
                    "NNUE_STORAGE_FAILED",
                    `ID mismatch: argument id="${id}" but meta.id="${meta.id}"`,
                );
            }

            // サイズチェック
            const size = data instanceof Blob ? data.size : data.byteLength;
            if (size > NNUE_MAX_SIZE_BYTES) {
                throw new NnueError(
                    "NNUE_SIZE_EXCEEDED",
                    `File size ${size} exceeds maximum ${NNUE_MAX_SIZE_BYTES}`,
                );
            }

            const blob = data instanceof Blob ? data : new Blob([data as BlobPart]);

            const db = await getNnueDB();
            const tx = db.transaction(["nnue-blobs", "nnue-meta"], "readwrite");

            try {
                await Promise.all([
                    tx.objectStore("nnue-blobs").put(blob, id),
                    tx.objectStore("nnue-meta").put(meta),
                    tx.done,
                ]);
            } catch (error) {
                throw mapIdbError(error);
            }
        },

        async load(id: string): Promise<Uint8Array> {
            const db = await getNnueDB();
            const blob = await db.get("nnue-blobs", id);

            if (!blob) {
                throw new NnueError("NNUE_NOT_FOUND", `NNUE not found: ${id}`);
            }

            return new Uint8Array(await blob.arrayBuffer());
        },

        async loadStream(id: string): Promise<ReadableStream<Uint8Array>> {
            const db = await getNnueDB();
            const blob = await db.get("nnue-blobs", id);

            if (!blob) {
                throw new NnueError("NNUE_NOT_FOUND", `NNUE not found: ${id}`);
            }

            return blob.stream();
        },

        async delete(id: string): Promise<void> {
            const db = await getNnueDB();
            const tx = db.transaction(["nnue-blobs", "nnue-meta"], "readwrite");

            try {
                await Promise.all([
                    tx.objectStore("nnue-blobs").delete(id),
                    tx.objectStore("nnue-meta").delete(id),
                    tx.done,
                ]);
            } catch (error) {
                throw new NnueError("NNUE_DELETE_FAILED", "Failed to delete NNUE", error);
            }
        },

        async listMeta(): Promise<NnueMeta[]> {
            const db = await getNnueDB();
            return db.getAll("nnue-meta");
        },

        async getMeta(id: string): Promise<NnueMeta | null> {
            const db = await getNnueDB();
            return (await db.get("nnue-meta", id)) ?? null;
        },

        async updateMeta(id: string, partial: Partial<NnueMeta>): Promise<void> {
            const db = await getNnueDB();
            const existing = await db.get("nnue-meta", id);

            if (!existing) {
                throw new NnueError("NNUE_NOT_FOUND", `NNUE not found: ${id}`);
            }

            const updated = { ...existing, ...partial, id }; // id は上書きさせない
            await db.put("nnue-meta", updated);
        },

        async getUsage(): Promise<{ used: number; quota?: number }> {
            if (navigator.storage?.estimate) {
                const estimate = await navigator.storage.estimate();
                return {
                    used: estimate.usage ?? 0,
                    quota: estimate.quota,
                };
            }
            return { used: 0 };
        },

        async listByContentHash(hash: string): Promise<NnueMeta[]> {
            const db = await getNnueDB();
            return db.getAllFromIndex("nnue-meta", "by-content-hash", hash);
        },

        async listByPresetKey(presetKey: string): Promise<NnueMeta[]> {
            const db = await getNnueDB();
            return db.getAllFromIndex("nnue-meta", "by-preset-key", presetKey);
        },
    };
}
