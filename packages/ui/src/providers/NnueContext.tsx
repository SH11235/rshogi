import type { NnueFormat, NnueMeta, NnueStorage } from "@shogi/app-core";
import { NnueError } from "@shogi/app-core";
import { createContext, type ReactNode, useCallback, useContext, useEffect, useState } from "react";

export interface NnueHeaderValidationResult {
    format?: NnueFormat;
    isCompatible: boolean;
}

export interface NnueContextValue {
    /** NNUE ストレージ実装 */
    storage: NnueStorage;
    /** NNUE メタデータ一覧（共有状態） */
    nnueList: NnueMeta[];
    /** 一覧読み込み中かどうか */
    isLoading: boolean;
    /** 直近のエラー */
    error: NnueError | null;
    /** 一覧を再取得 */
    refreshList: () => Promise<void>;
    /** ストレージ使用量 */
    storageUsage: { used: number; quota?: number } | null;
    /** エラーをクリア */
    clearError: () => void;
    /** NNUE ヘッダ検証（任意） */
    validateNnueHeader?: (header: Uint8Array) => Promise<NnueHeaderValidationResult>;
}

const NnueContext = createContext<NnueContextValue | null>(null);

export interface NnueProviderProps {
    /** NNUE ストレージ実装 */
    storage: NnueStorage;
    /** NNUE ヘッダ検証（任意） */
    validateNnueHeader?: (header: Uint8Array) => Promise<NnueHeaderValidationResult>;
    children: ReactNode;
}

/**
 * NNUE ストレージを提供する Provider
 *
 * Web と Desktop で異なる NnueStorage 実装を注入できる。
 * nnueList は Context レベルで共有され、全てのコンポーネントで同期される。
 *
 * @example
 * ```tsx
 * // Web (IndexedDB)
 * import { createIndexedDBNnueStorage } from "@shogi/engine-wasm";
 * const storage = await createIndexedDBNnueStorage();
 * <NnueProvider storage={storage}>...</NnueProvider>
 *
 * // Desktop (Tauri)
 * import { createTauriNnueStorage } from "@shogi/engine-tauri";
 * const storage = await createTauriNnueStorage();
 * <NnueProvider storage={storage}>...</NnueProvider>
 * ```
 */
export function NnueProvider({
    storage,
    validateNnueHeader,
    children,
}: NnueProviderProps): ReactNode {
    const [nnueList, setNnueList] = useState<NnueMeta[]>([]);
    const [isLoading, setIsLoading] = useState(true);
    const [error, setError] = useState<NnueError | null>(null);
    const [storageUsage, setStorageUsage] = useState<{ used: number; quota?: number } | null>(null);

    const refreshList = useCallback(async () => {
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

    const clearError = useCallback(() => {
        setError(null);
    }, []);

    // 初回マウント時に一覧を取得
    useEffect(() => {
        void refreshList();
    }, [refreshList]);

    return (
        <NnueContext.Provider
            value={{
                storage,
                nnueList,
                isLoading,
                error,
                refreshList,
                storageUsage,
                clearError,
                validateNnueHeader,
            }}
        >
            {children}
        </NnueContext.Provider>
    );
}

/**
 * NnueContext を取得するフック
 *
 * NnueProvider の外で使用するとエラーを投げる。
 */
export function useNnueContext(): NnueContextValue {
    const context = useContext(NnueContext);
    if (!context) {
        throw new Error("useNnueContext must be used within a NnueProvider");
    }
    return context;
}

/**
 * NnueContext を取得するフック（Optional版）
 *
 * NnueProvider の外で使用した場合は null を返す。
 */
export function useNnueContextOptional(): NnueContextValue | null {
    return useContext(NnueContext);
}
