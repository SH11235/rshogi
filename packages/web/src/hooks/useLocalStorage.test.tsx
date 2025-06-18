import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useLocalStorage } from "./useLocalStorage";

// localStorage のモック
const mockLocalStorage = (() => {
    let store: Record<string, string> = {};
    return {
        getItem: vi.fn((key: string) => store[key] || null),
        setItem: vi.fn((key: string, value: string) => {
            store[key] = value;
        }),
        removeItem: vi.fn((key: string) => {
            delete store[key];
        }),
        clear: vi.fn(() => {
            store = {};
        }),
    };
})();

Object.defineProperty(window, "localStorage", {
    value: mockLocalStorage,
});

describe("useLocalStorage", () => {
    beforeEach(() => {
        mockLocalStorage.clear();
        vi.clearAllMocks();
    });

    it("should return default value when localStorage is empty", () => {
        const { result } = renderHook(() => useLocalStorage("test-key", "default"));

        expect(result.current[0]).toBe("default");
        expect(mockLocalStorage.getItem).toHaveBeenCalledWith("test-key");
    });

    it("should return stored value when localStorage has data", () => {
        mockLocalStorage.setItem("test-key", JSON.stringify("stored-value"));

        const { result } = renderHook(() => useLocalStorage("test-key", "default"));

        expect(result.current[0]).toBe("stored-value");
    });

    it("should update localStorage when value is set", () => {
        const { result } = renderHook(() => useLocalStorage("test-key", "default"));

        act(() => {
            result.current[1]("new-value");
        });

        expect(result.current[0]).toBe("new-value");
        expect(mockLocalStorage.setItem).toHaveBeenCalledWith(
            "test-key",
            JSON.stringify("new-value"),
        );
    });

    it("should support function updater", () => {
        const { result } = renderHook(() => useLocalStorage("test-key", 0));

        act(() => {
            result.current[1]((prev) => prev + 1);
        });

        expect(result.current[0]).toBe(1);
        expect(mockLocalStorage.setItem).toHaveBeenCalledWith("test-key", JSON.stringify(1));
    });

    it("should remove value from localStorage", () => {
        const { result } = renderHook(() => useLocalStorage("test-key", "default"));

        act(() => {
            result.current[1]("new-value");
        });

        expect(result.current[0]).toBe("new-value");

        act(() => {
            result.current[2](); // removeValue
        });

        expect(result.current[0]).toBe("default");
        expect(mockLocalStorage.removeItem).toHaveBeenCalledWith("test-key");
    });

    it("should handle complex objects", () => {
        const defaultObject = { name: "test", count: 0 };
        const newObject = { name: "updated", count: 5 };

        const { result } = renderHook(() => useLocalStorage("test-key", defaultObject));

        act(() => {
            result.current[1](newObject);
        });

        expect(result.current[0]).toEqual(newObject);
        expect(mockLocalStorage.setItem).toHaveBeenCalledWith(
            "test-key",
            JSON.stringify(newObject),
        );
    });

    it("should handle JSON parse errors gracefully", () => {
        // 不正なJSONを設定
        mockLocalStorage.setItem("test-key", "invalid-json");

        const consoleSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

        const { result } = renderHook(() => useLocalStorage("test-key", "default"));

        expect(result.current[0]).toBe("default");
        expect(consoleSpy).toHaveBeenCalled();

        consoleSpy.mockRestore();
    });

    it("should handle localStorage errors gracefully", () => {
        const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

        // setItem でエラーを発生させる
        mockLocalStorage.setItem.mockImplementationOnce(() => {
            throw new Error("Storage quota exceeded");
        });

        const { result } = renderHook(() => useLocalStorage("test-key", "default"));

        act(() => {
            result.current[1]("new-value");
        });

        expect(consoleErrorSpy).toHaveBeenCalled();

        consoleErrorSpy.mockRestore();
    });
});
