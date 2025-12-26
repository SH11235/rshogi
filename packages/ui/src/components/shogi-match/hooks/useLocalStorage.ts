import { useEffect, useState } from "react";

/**
 * localStorage と同期する useState フック
 *
 * @param key - localStorage のキー
 * @param defaultValue - デフォルト値（localStorage に値がない場合に使用）
 * @returns [value, setValue] - useState と同じインターフェース
 */
export function useLocalStorage<T>(
    key: string,
    defaultValue: T,
): [T, (value: T | ((prev: T) => T)) => void] {
    // 初期値を localStorage から読み込む
    const [value, setValue] = useState<T>(() => {
        if (typeof window === "undefined") {
            return defaultValue;
        }
        try {
            const stored = localStorage.getItem(key);
            if (stored === null) {
                return defaultValue;
            }
            return JSON.parse(stored) as T;
        } catch (error) {
            console.warn(`Failed to parse localStorage key "${key}":`, error);
            return defaultValue;
        }
    });

    // 値が変更されたら localStorage に保存
    useEffect(() => {
        if (typeof window === "undefined") {
            return;
        }
        try {
            localStorage.setItem(key, JSON.stringify(value));
        } catch (error) {
            // LocalStorage容量制限（通常5-10MB）に達した場合のハンドリング
            if (error instanceof DOMException && error.name === "QuotaExceededError") {
                console.error(
                    `LocalStorage quota exceeded for key "${key}". Consider clearing old data.`,
                );
            } else {
                console.warn(`Failed to save to localStorage key "${key}":`, error);
            }
        }
    }, [key, value]);

    // useStateのsetValueは安定した参照なので直接返す
    return [value, setValue];
}
