/**
 * Tauri 向け NNUE ストレージ実装
 *
 * ファイルシステムベースの NNUE 管理
 * - NNUE ファイルは app_data_dir/nnue/ に保存
 * - メタデータは localStorage に保存（軽量なため）
 */

import { generateNnueId, type NnueMeta, type NnueStorage } from "@shogi/app-core";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/**
 * チャンクサイズ（1MB）
 * base64 変換後は約 1.33MB
 */
const CHUNK_SIZE = 1 * 1024 * 1024;
const BINARY_STRING_CHUNK_SIZE = 0x8000;

// バイナリ文字列はバイトをそのまま保持できる方法で生成する
function bytesToBinaryString(bytes: Uint8Array): string {
    let result = "";
    for (let i = 0; i < bytes.length; i += BINARY_STRING_CHUNK_SIZE) {
        const slice = bytes.subarray(i, i + BINARY_STRING_CHUNK_SIZE);
        result += String.fromCharCode(...slice);
    }
    return result;
}

/**
 * Uint8Array の一部を base64 文字列に変換
 * チャンク単位で変換することでメモリ効率を改善
 */
function chunkToBase64(bytes: Uint8Array, offset: number, length: number): string {
    const chunk = bytes.subarray(offset, offset + length);
    return btoa(bytesToBinaryString(chunk));
}

type InvokeFn = typeof tauriInvoke;

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
        capabilities: {
            supportsFileImport: false, // 将来 true に変更可能（Tauri drag&drop API対応時）
            supportsPathImport: true, // Tauri ダイアログでパス取得
            supportsLoad: false, // Rust 側でファイルパス直接使用
        },

        async save(id: string, data: Blob | Uint8Array, meta: NnueMeta): Promise<void> {
            const bytes = data instanceof Blob ? new Uint8Array(await data.arrayBuffer()) : data;

            try {
                // チャンク単位で送信（1MB ずつ）
                const totalChunks = Math.ceil(bytes.length / CHUNK_SIZE);
                for (let chunkIndex = 0; chunkIndex < totalChunks; chunkIndex++) {
                    const offset = chunkIndex * CHUNK_SIZE;
                    const length = Math.min(CHUNK_SIZE, bytes.length - offset);
                    const dataBase64 = chunkToBase64(bytes, offset, length);

                    await invoke("save_nnue_chunk", { id, chunkIndex, dataBase64 });
                }

                // 保存を完了
                await invoke("finalize_nnue_save", { id });

                // メタデータを保存
                metaCache.set(id, meta);
                saveMetaToStorage(metaCache);
            } catch (error) {
                // エラー時は一時ファイルを削除
                await invoke("abort_nnue_save", { id }).catch(() => {
                    // 中止処理のエラーは無視
                });
                throw error;
            }
        },

        // load / loadStream は capabilities.supportsLoad === false のため未定義

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

        async importFromPath(srcPath: string, displayName?: string): Promise<NnueMeta> {
            const id = generateNnueId();

            // ファイルをコピー
            const result = await importNnueFromPath(srcPath, id, { invoke });

            // SHA-256 計算
            const hash = await calculateNnueHash(id, { invoke });

            // ファイル名を抽出
            const fileName = srcPath.split(/[/\\]/).pop() ?? "unknown.nnue";

            // 重複チェック
            const existing = Array.from(metaCache.values()).filter(
                (m) => m.contentHashSha256 === hash,
            );
            if (existing.length > 0) {
                // 重複ファイルを削除して既存のメタを返す
                await invoke("delete_nnue", { id });
                return existing[0];
            }

            const meta: NnueMeta = {
                id,
                displayName: displayName ?? fileName.replace(/\.nnue$/i, ""),
                originalFileName: fileName,
                size: result.size,
                contentHashSha256: hash,
                source: "user-uploaded",
                createdAt: Date.now(),
                verified: false,
            };

            // メタデータを保存
            metaCache.set(id, meta);
            saveMetaToStorage(metaCache);

            return meta;
        },
    };
}

/**
 * NNUE ファイルのパスを取得
 */
async function getNnuePath(id: string, options: { invoke?: InvokeFn } = {}): Promise<string> {
    const invoke = options.invoke ?? tauriInvoke;
    return invoke<string>("get_nnue_path", { id });
}

/**
 * NNUE ファイルのハッシュを計算
 */
async function calculateNnueHash(id: string, options: { invoke?: InvokeFn } = {}): Promise<string> {
    const invoke = options.invoke ?? tauriInvoke;
    return invoke<string>("calculate_nnue_hash", { id });
}

/**
 * ファイルパスから NNUE をインポート
 * @param srcPath ソースファイルのパス
 * @param id 識別子（UUID）
 */
async function importNnueFromPath(
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
async function importNnue(
    srcPath: string,
    displayName?: string,
    options: { invoke?: InvokeFn } = {},
): Promise<NnueMeta> {
    const invoke = options.invoke ?? tauriInvoke;
    const id = generateNnueId();

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
