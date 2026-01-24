import type { NnueMeta, NnueStorageCapabilities } from "@shogi/app-core";
import { NnueError } from "@shogi/app-core";
import { useCallback, useState } from "react";
import { useNnueContextOptional } from "../providers/NnueContext";

export interface UseNnueStorageReturn {
    /** NNUE メタデータ一覧 */
    nnueList: NnueMeta[];
    /** 一覧読み込み中かどうか */
    isLoading: boolean;
    /** 直近のエラー */
    error: NnueError | null;
    /** 一覧を再取得 */
    refreshList: () => Promise<void>;
    /** ファイルから NNUE をインポート（capabilities.supportsFileImport === true の場合） */
    importFromFile: (file: File) => Promise<NnueMeta>;
    /** パスから NNUE をインポート（capabilities.supportsPathImport === true の場合） */
    importFromPath: (srcPath: string, displayName?: string) => Promise<NnueMeta>;
    /** NNUE を削除 */
    deleteNnue: (id: string) => Promise<void>;
    /** NNUE の表示名を更新 */
    updateDisplayName: (id: string, displayName: string) => Promise<void>;
    /** エラーをクリア */
    clearError: () => void;
    /** ストレージ使用量 */
    storageUsage: { used: number; quota?: number } | null;
    /** ストレージの capability */
    capabilities: NnueStorageCapabilities | null;
}

/**
 * NNUE ストレージを操作するフック
 *
 * NnueProvider 経由で注入された NnueStorage を React 状態として管理する。
 * nnueList は Context レベルで共有され、全てのコンポーネントで同期される。
 * NnueProvider の外で使用すると、空の状態を返す。
 */
export function useNnueStorage(): UseNnueStorageReturn {
    const context = useNnueContextOptional();
    const storage = context?.storage ?? null;
    const capabilities = storage?.capabilities ?? null;

    // Context から共有状態を取得
    const nnueList = context?.nnueList ?? [];
    const contextIsLoading = context?.isLoading ?? false;
    const contextError = context?.error ?? null;
    const contextRefreshList = context?.refreshList;
    const storageUsage = context?.storageUsage ?? null;
    const contextClearError = context?.clearError;

    // ローカルの操作中状態（インポート・削除の進行中）
    const [isOperating, setIsOperating] = useState(false);
    const [localError, setLocalError] = useState<NnueError | null>(null);

    // isLoading は Context のローディング状態とローカルの操作状態を組み合わせ
    const isLoading = contextIsLoading || isOperating;

    // エラーはローカルエラーを優先、なければ Context のエラー
    const error = localError ?? contextError;

    const refreshList = useCallback(async () => {
        if (contextRefreshList) {
            await contextRefreshList();
        }
    }, [contextRefreshList]);

    const importFromFile = useCallback(
        async (file: File): Promise<NnueMeta> => {
            if (!storage) {
                throw new NnueError(
                    "NNUE_STORAGE_FAILED",
                    "NnueProvider が設定されていません",
                    null,
                );
            }
            if (!storage.capabilities.supportsFileImport) {
                throw new NnueError(
                    "NNUE_STORAGE_FAILED",
                    "このプラットフォームでは File からのインポートがサポートされていません",
                    null,
                );
            }
            setIsOperating(true);
            setLocalError(null);
            try {
                const arrayBuffer = await file.arrayBuffer();
                const data = new Uint8Array(arrayBuffer);

                // SHA-256 ハッシュを計算
                const hashBuffer = await crypto.subtle.digest("SHA-256", data);
                const hashArray = Array.from(new Uint8Array(hashBuffer));
                const hash = hashArray.map((b) => b.toString(16).padStart(2, "0")).join("");

                // 重複チェック
                const existing = await storage.listByContentHash(hash);
                if (existing.length > 0) {
                    // 既存のものを返す（重複保存しない）
                    return existing[0];
                }

                // ID を生成
                const id = crypto.randomUUID();

                // メタデータを作成
                const meta: NnueMeta = {
                    id,
                    displayName: file.name.replace(/\.(nnue|bin)$/i, ""),
                    originalFileName: file.name,
                    size: data.byteLength,
                    contentHashSha256: hash,
                    source: "user-uploaded",
                    createdAt: Date.now(),
                    verified: false,
                };

                // 保存
                await storage.save(id, data, meta);
                await refreshList();
                return meta;
            } catch (e) {
                const err =
                    e instanceof NnueError
                        ? e
                        : new NnueError(
                              "NNUE_STORAGE_FAILED",
                              "NNUE のインポートに失敗しました",
                              e,
                          );
                setLocalError(err);
                throw err;
            } finally {
                setIsOperating(false);
            }
        },
        [storage, refreshList],
    );

    const importFromPath = useCallback(
        async (srcPath: string, displayName?: string): Promise<NnueMeta> => {
            if (!storage) {
                throw new NnueError(
                    "NNUE_STORAGE_FAILED",
                    "NnueProvider が設定されていません",
                    null,
                );
            }
            if (!storage.capabilities.supportsPathImport || !storage.importFromPath) {
                throw new NnueError(
                    "NNUE_STORAGE_FAILED",
                    "このプラットフォームではパスからのインポートがサポートされていません",
                    null,
                );
            }
            setIsOperating(true);
            setLocalError(null);
            try {
                const meta = await storage.importFromPath(srcPath, displayName);
                await refreshList();
                return meta;
            } catch (e) {
                const err =
                    e instanceof NnueError
                        ? e
                        : new NnueError(
                              "NNUE_STORAGE_FAILED",
                              "NNUE のインポートに失敗しました",
                              e,
                          );
                setLocalError(err);
                throw err;
            } finally {
                setIsOperating(false);
            }
        },
        [storage, refreshList],
    );

    const deleteNnue = useCallback(
        async (id: string) => {
            if (!storage) {
                throw new NnueError(
                    "NNUE_STORAGE_FAILED",
                    "NnueProvider が設定されていません",
                    null,
                );
            }
            setIsOperating(true);
            setLocalError(null);
            try {
                await storage.delete(id);
                await refreshList();
            } catch (e) {
                const err =
                    e instanceof NnueError
                        ? e
                        : new NnueError("NNUE_DELETE_FAILED", "NNUE の削除に失敗しました", e);
                setLocalError(err);
                throw err;
            } finally {
                setIsOperating(false);
            }
        },
        [storage, refreshList],
    );

    const updateDisplayName = useCallback(
        async (id: string, displayName: string) => {
            if (!storage) {
                throw new NnueError(
                    "NNUE_STORAGE_FAILED",
                    "NnueProvider が設定されていません",
                    null,
                );
            }
            setLocalError(null);
            try {
                await storage.updateMeta(id, { displayName });
                await refreshList();
            } catch (e) {
                const err =
                    e instanceof NnueError
                        ? e
                        : new NnueError("NNUE_STORAGE_FAILED", "表示名の更新に失敗しました", e);
                setLocalError(err);
                throw err;
            }
        },
        [storage, refreshList],
    );

    const clearError = useCallback(() => {
        setLocalError(null);
        contextClearError?.();
    }, [contextClearError]);

    return {
        nnueList,
        isLoading,
        error,
        refreshList,
        importFromFile,
        importFromPath,
        deleteNnue,
        updateDisplayName,
        clearError,
        storageUsage,
        capabilities,
    };
}
