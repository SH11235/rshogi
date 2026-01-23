import type { NnueMeta } from "@shogi/app-core";
import { NnueError } from "@shogi/app-core";
import { useCallback, useEffect, useState } from "react";
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
    /** ファイルから NNUE をインポート */
    importFromFile: (file: File) => Promise<NnueMeta>;
    /** NNUE を削除 */
    deleteNnue: (id: string) => Promise<void>;
    /** エラーをクリア */
    clearError: () => void;
    /** ストレージ使用量 */
    storageUsage: { used: number; quota?: number } | null;
}

/**
 * NNUE ストレージを操作するフック
 *
 * NnueProvider 経由で注入された NnueStorage を React 状態として管理する。
 * NnueProvider の外で使用すると、空の状態を返す。
 */
export function useNnueStorage(): UseNnueStorageReturn {
    const context = useNnueContextOptional();
    const storage = context?.storage ?? null;

    const [nnueList, setNnueList] = useState<NnueMeta[]>([]);
    const [isLoading, setIsLoading] = useState(false);
    const [error, setError] = useState<NnueError | null>(null);
    const [storageUsage, setStorageUsage] = useState<{ used: number; quota?: number } | null>(null);

    const refreshList = useCallback(async () => {
        if (!storage) return;
        setIsLoading(true);
        try {
            const [list, usage] = await Promise.all([storage.listMeta(), storage.getUsage()]);
            // ソート: 作成日時の新しい順
            list.sort((a, b) => b.createdAt - a.createdAt);
            setNnueList(list);
            setStorageUsage(usage);
            setError(null);
        } catch (e) {
            const err =
                e instanceof NnueError
                    ? e
                    : new NnueError("NNUE_STORAGE_FAILED", "NNUE 一覧の取得に失敗しました", e);
            setError(err);
        } finally {
            setIsLoading(false);
        }
    }, [storage]);

    useEffect(() => {
        void refreshList();
    }, [refreshList]);

    const importFromFile = useCallback(
        async (file: File): Promise<NnueMeta> => {
            if (!storage) {
                throw new NnueError(
                    "NNUE_STORAGE_FAILED",
                    "NnueProvider が設定されていません",
                    null,
                );
            }
            setIsLoading(true);
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
                    setError(null);
                    return existing[0];
                }

                // ID を生成
                const id = crypto.randomUUID();

                // メタデータを作成
                const meta: NnueMeta = {
                    id,
                    displayName: file.name.replace(/\.nnue$/i, ""),
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
                setError(null);
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
                setError(err);
                throw err;
            } finally {
                setIsLoading(false);
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
            setIsLoading(true);
            try {
                await storage.delete(id);
                await refreshList();
                setError(null);
            } catch (e) {
                const err =
                    e instanceof NnueError
                        ? e
                        : new NnueError("NNUE_DELETE_FAILED", "NNUE の削除に失敗しました", e);
                setError(err);
                throw err;
            } finally {
                setIsLoading(false);
            }
        },
        [storage, refreshList],
    );

    const clearError = useCallback(() => {
        setError(null);
    }, []);

    return {
        nnueList,
        isLoading,
        error,
        refreshList,
        importFromFile,
        deleteNnue,
        clearError,
        storageUsage,
    };
}
