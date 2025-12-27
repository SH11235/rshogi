/**
 * 開発者モードフック
 *
 * URLパラメータ + localStorage で開発者向けUIの表示を制御
 *
 * 使い方:
 * - `?dev=true` でアクセス → 開発者モードON（localStorage に保存）
 * - `?dev=false` でアクセス → 開発者モードOFF（localStorage から削除）
 * - パラメータなし → localStorage の値を使用
 */

import { useEffect, useState } from "react";

const STORAGE_KEY = "shogi-dev-mode";

/**
 * URLパラメータから開発者モードの設定を取得
 * @returns true: ON, false: OFF, null: パラメータなし
 */
function getDevModeFromUrl(): boolean | null {
    if (typeof window === "undefined") return null;

    const params = new URLSearchParams(window.location.search);
    const devParam = params.get("dev");

    if (devParam === "true" || devParam === "1") {
        return true;
    }
    if (devParam === "false" || devParam === "0") {
        return false;
    }
    return null;
}

/**
 * localStorage から開発者モードの設定を取得
 */
function getDevModeFromStorage(): boolean {
    if (typeof window === "undefined") return false;

    try {
        return localStorage.getItem(STORAGE_KEY) === "true";
    } catch {
        return false;
    }
}

/**
 * localStorage に開発者モードの設定を保存
 */
function setDevModeToStorage(enabled: boolean): void {
    if (typeof window === "undefined") return;

    try {
        if (enabled) {
            localStorage.setItem(STORAGE_KEY, "true");
        } else {
            localStorage.removeItem(STORAGE_KEY);
        }
    } catch {
        // localStorage が使えない場合は無視
    }
}

/**
 * 開発者モードフック
 *
 * @returns 開発者モードが有効かどうか
 */
export function useDevMode(): boolean {
    const [isDevMode, setIsDevMode] = useState<boolean>(() => {
        // 初期値: URLパラメータ優先、なければ localStorage
        const fromUrl = getDevModeFromUrl();
        if (fromUrl !== null) {
            return fromUrl;
        }
        return getDevModeFromStorage();
    });

    useEffect(() => {
        const fromUrl = getDevModeFromUrl();

        if (fromUrl !== null) {
            // URLパラメータがある場合は localStorage に保存
            setDevModeToStorage(fromUrl);
            setIsDevMode(fromUrl);

            // URLからパラメータを削除（履歴を汚さないため）
            const url = new URL(window.location.href);
            url.searchParams.delete("dev");
            window.history.replaceState({}, "", url.toString());
        }
    }, []);

    return isDevMode;
}
