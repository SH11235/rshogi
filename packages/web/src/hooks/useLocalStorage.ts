import { useCallback, useEffect, useState } from "react";

/**
 * localStorage を React state と同期する hook
 * @param key localStorage のキー
 * @param defaultValue デフォルト値
 * @returns [value, setValue, removeValue] のタプル
 */
export function useLocalStorage<T>(
    key: string,
    defaultValue: T,
): [T, (value: T | ((prev: T) => T)) => void, () => void] {
    // 初期値の読み込み
    const [value, setValue] = useState<T>(() => {
        if (typeof window === "undefined") {
            return defaultValue;
        }

        try {
            const item = window.localStorage.getItem(key);
            if (item === null) {
                return defaultValue;
            }
            return JSON.parse(item) as T;
        } catch (error) {
            console.warn(`Error reading localStorage key "${key}":`, error);
            return defaultValue;
        }
    });

    // localStorage への保存
    const setStoredValue = useCallback(
        (newValue: T | ((prev: T) => T)) => {
            try {
                const valueToStore = newValue instanceof Function ? newValue(value) : newValue;
                setValue(valueToStore);

                if (typeof window !== "undefined") {
                    window.localStorage.setItem(key, JSON.stringify(valueToStore));
                }
            } catch (error) {
                console.error(`Error setting localStorage key "${key}":`, error);
            }
        },
        [key, value],
    );

    // localStorage からの削除
    const removeStoredValue = useCallback(() => {
        try {
            setValue(defaultValue);
            if (typeof window !== "undefined") {
                window.localStorage.removeItem(key);
            }
        } catch (error) {
            console.error(`Error removing localStorage key "${key}":`, error);
        }
    }, [key, defaultValue]);

    // storage イベントの監視（他のタブでの変更を検知）
    useEffect(() => {
        const handleStorageChange = (e: StorageEvent) => {
            if (e.key === key && e.newValue !== null) {
                try {
                    setValue(JSON.parse(e.newValue) as T);
                } catch (error) {
                    console.warn(`Error parsing storage event for key "${key}":`, error);
                }
            }
        };

        window.addEventListener("storage", handleStorageChange);
        return () => {
            window.removeEventListener("storage", handleStorageChange);
        };
    }, [key]);

    return [value, setStoredValue, removeStoredValue];
}
