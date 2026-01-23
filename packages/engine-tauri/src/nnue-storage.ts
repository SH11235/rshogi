/**
 * Tauri 向け NNUE ストレージ実装
 *
 * ファイルシステムベースの NNUE 管理
 * - NNUE ファイルは app_data_dir/nnue/ に保存
 * - メタデータは localStorage に保存（軽量なため）
 */

import type { NnueMeta, NnueStorage } from "@shogi/app-core";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export type InvokeFn = typeof tauriInvoke;

export interface TauriNnueStorageOptions {
    /**
     * IPC 実装を差し替える場合に指定 (テスト用)
     */
    invoke?: InvokeFn;
}

interface NnueImportResult {
    id: string;
    size: number;
    path: string;
}

const META_STORAGE_KEY = "shogi-nnue-meta";

/**
 * localStorage からメタデータを読み込む
 */
function loadMetaFromStorage(): Map<string, NnueMeta> {
    try {
        const data = localStorage.getItem(META_STORAGE_KEY);
        if (!data) return new Map();
        const arr = JSON.parse(data) as NnueMeta[];
        return new Map(arr.map((m) => [m.id, m]));
    } catch {
        return new Map();
    }
}

/**
 * localStorage にメタデータを保存
 */
function saveMetaToStorage(meta: Map<string, NnueMeta>): void {
    const arr = Array.from(meta.values());
    localStorage.setItem(META_STORAGE_KEY, JSON.stringify(arr));
}

/**
 * Tauri 向け NnueStorage を作成
 */
export function createTauriNnueStorage(options: TauriNnueStorageOptions = {}): NnueStorage {
    const invoke = options.invoke ?? tauriInvoke;
    const metaCache = loadMetaFromStorage();

    return {
        async save(_id: string, _data: Blob | Uint8Array, _meta: NnueMeta): Promise<void> {
            // Tauri では save は使わない（import_nnue_from_path を使う）
            // この関数は URL からダウンロードした場合に使用
            // 一時ファイルに書き出してから import するか、
            // または Tauri 側に専用のコマンドを追加する必要がある
            throw new Error("Direct save is not supported in Tauri. Use importNnueFile() instead.");
        },

        async load(_id: string): Promise<Uint8Array> {
            // Tauri では Rust 側でファイルを読み込むため、
            // 通常は使用しない（エンジンに直接パスを渡す）
            throw new Error(
                "Direct load is not supported in Tauri. Use getNnuePath() and pass to engine.",
            );
        },

        async loadStream(_id: string): Promise<ReadableStream<Uint8Array>> {
            // Tauri では使用しない
            throw new Error("Stream load is not supported in Tauri.");
        },

        async delete(id: string): Promise<void> {
            await invoke("delete_nnue", { id });
            metaCache.delete(id);
            saveMetaToStorage(metaCache);
        },

        async listMeta(): Promise<NnueMeta[]> {
            // ファイルシステムと localStorage の整合性を確認
            const fileIds = await invoke<string[]>("list_nnue_files");
            const fileIdSet = new Set(fileIds);

            // ファイルが存在しないメタデータを削除
            let changed = false;
            for (const id of metaCache.keys()) {
                if (!fileIdSet.has(id)) {
                    metaCache.delete(id);
                    changed = true;
                }
            }
            if (changed) {
                saveMetaToStorage(metaCache);
            }

            return Array.from(metaCache.values());
        },

        async getMeta(id: string): Promise<NnueMeta | null> {
            return metaCache.get(id) ?? null;
        },

        async updateMeta(id: string, partial: Partial<NnueMeta>): Promise<void> {
            const existing = metaCache.get(id);
            if (!existing) {
                throw new Error(`NNUE not found: ${id}`);
            }
            const updated = { ...existing, ...partial };
            metaCache.set(id, updated);
            saveMetaToStorage(metaCache);
        },

        async getUsage(): Promise<{ used: number; quota?: number }> {
            // ファイルシステムの使用量を計算
            const metas = await this.listMeta();
            const used = metas.reduce((sum, m) => sum + m.size, 0);
            return { used };
        },

        async listByContentHash(hash: string): Promise<NnueMeta[]> {
            return Array.from(metaCache.values()).filter((m) => m.contentHashSha256 === hash);
        },

        async listByPresetKey(presetKey: string): Promise<NnueMeta[]> {
            return Array.from(metaCache.values()).filter((m) => m.presetKey === presetKey);
        },
    };
}

/**
 * NNUE ファイルのパスを取得
 */
export async function getNnuePath(
    id: string,
    options: { invoke?: InvokeFn } = {},
): Promise<string> {
    const invoke = options.invoke ?? tauriInvoke;
    return invoke<string>("get_nnue_path", { id });
}

/**
 * NNUE ファイルのハッシュを計算
 */
export async function calculateNnueHash(
    id: string,
    options: { invoke?: InvokeFn } = {},
): Promise<string> {
    const invoke = options.invoke ?? tauriInvoke;
    return invoke<string>("calculate_nnue_hash", { id });
}

/**
 * ファイルパスから NNUE をインポート
 * @param srcPath ソースファイルのパス
 * @param id 識別子（UUID）
 */
export async function importNnueFromPath(
    srcPath: string,
    id: string,
    options: { invoke?: InvokeFn } = {},
): Promise<NnueImportResult> {
    const invoke = options.invoke ?? tauriInvoke;
    return invoke<NnueImportResult>("import_nnue_from_path", {
        srcPath,
        id,
    });
}

/**
 * NNUE ファイルをインポートしてメタデータを作成
 * @param srcPath ソースファイルのパス
 * @param displayName 表示名（省略時はファイル名）
 */
export async function importNnue(
    srcPath: string,
    displayName?: string,
    options: { invoke?: InvokeFn } = {},
): Promise<NnueMeta> {
    const invoke = options.invoke ?? tauriInvoke;
    const id = crypto.randomUUID();

    // ファイルをコピー
    const result = await importNnueFromPath(srcPath, id, { invoke });

    // SHA-256 計算
    const hash = await calculateNnueHash(id, { invoke });

    // ファイル名を抽出
    const fileName = srcPath.split(/[/\\]/).pop() ?? "unknown.nnue";

    const meta: NnueMeta = {
        id,
        displayName: displayName ?? fileName,
        originalFileName: fileName,
        size: result.size,
        contentHashSha256: hash,
        source: "user-uploaded",
        createdAt: Date.now(),
        verified: false,
    };

    // メタデータを保存
    const metaCache = loadMetaFromStorage();
    metaCache.set(id, meta);
    saveMetaToStorage(metaCache);

    return meta;
}
