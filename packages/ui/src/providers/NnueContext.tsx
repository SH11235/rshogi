import type { NnueStorage } from "@shogi/app-core";
import { createContext, type ReactNode, useContext } from "react";

export type NnuePlatform = "web" | "desktop";

export interface NnueContextValue {
    /** NNUE ストレージ実装 */
    storage: NnueStorage;
    /** プラットフォーム種別 */
    platform: NnuePlatform;
}

const NnueContext = createContext<NnueContextValue | null>(null);

export interface NnueProviderProps {
    /** NNUE ストレージ実装 */
    storage: NnueStorage;
    /** プラットフォーム種別 */
    platform: NnuePlatform;
    children: ReactNode;
}

/**
 * NNUE ストレージを提供する Provider
 *
 * Web と Desktop で異なる NnueStorage 実装を注入できる。
 *
 * @example
 * ```tsx
 * // Web (IndexedDB)
 * import { createIndexedDBNnueStorage } from "@shogi/engine-wasm";
 * const storage = await createIndexedDBNnueStorage();
 * <NnueProvider storage={storage} platform="web">...</NnueProvider>
 *
 * // Desktop (Tauri)
 * import { createTauriNnueStorage } from "@shogi/engine-tauri";
 * const storage = await createTauriNnueStorage();
 * <NnueProvider storage={storage} platform="desktop">...</NnueProvider>
 * ```
 */
export function NnueProvider({ storage, platform, children }: NnueProviderProps): ReactNode {
    return <NnueContext.Provider value={{ storage, platform }}>{children}</NnueContext.Provider>;
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
