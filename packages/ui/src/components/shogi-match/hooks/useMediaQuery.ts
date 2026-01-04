import { useEffect, useState } from "react";

/**
 * メディアクエリの状態を監視するフック
 * @param query - CSS メディアクエリ文字列 (例: '(max-width: 767px)')
 * @returns マッチしているかどうか
 */
export function useMediaQuery(query: string): boolean {
    const [matches, setMatches] = useState(false);

    useEffect(() => {
        if (typeof window === "undefined") return;

        const mql = window.matchMedia(query);
        setMatches(mql.matches);

        const handler = (e: MediaQueryListEvent) => setMatches(e.matches);
        mql.addEventListener("change", handler);
        return () => mql.removeEventListener("change", handler);
    }, [query]);

    return matches;
}

/**
 * モバイル判定用ブレークポイント (768px未満)
 */
export const MOBILE_BREAKPOINT = 768;

/**
 * モバイル表示かどうかを判定するフック
 * @returns モバイル表示の場合 true
 */
export function useIsMobile(): boolean {
    return useMediaQuery(`(max-width: ${MOBILE_BREAKPOINT - 1}px)`);
}
