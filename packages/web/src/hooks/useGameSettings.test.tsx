import { defaultGameSettings } from "@/types/settings";
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useGameSettings } from "./useGameSettings";

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

describe("useGameSettings", () => {
    beforeEach(() => {
        mockLocalStorage.clear();
        vi.clearAllMocks();
    });

    it("should return default settings initially", () => {
        const { result } = renderHook(() => useGameSettings());

        expect(result.current.settings).toEqual(defaultGameSettings);
        expect(result.current.isTimeControlEnabled).toBe(false);
        expect(result.current.currentTheme).toBe("auto");
        expect(result.current.isSoundEnabled).toBe(true);
    });

    it("should update time control settings", () => {
        const { result } = renderHook(() => useGameSettings());

        act(() => {
            result.current.updateTimeControl({
                enabled: true,
                mainTimeMinutes: 30,
            });
        });

        expect(result.current.settings.timeControl.enabled).toBe(true);
        expect(result.current.settings.timeControl.mainTimeMinutes).toBe(30);
        expect(result.current.settings.timeControl.byoyomiSeconds).toBe(30); // unchanged
        expect(result.current.isTimeControlEnabled).toBe(true);
    });

    it("should update audio settings", () => {
        const { result } = renderHook(() => useGameSettings());

        act(() => {
            result.current.updateAudio({
                masterVolume: 75,
                pieceSound: false,
            });
        });

        expect(result.current.settings.audio.masterVolume).toBe(75);
        expect(result.current.settings.audio.pieceSound).toBe(false);
        expect(result.current.settings.audio.checkSound).toBe(true); // unchanged
        expect(result.current.isSoundEnabled).toBe(true);
    });

    it("should detect when sound is disabled", () => {
        const { result } = renderHook(() => useGameSettings());

        act(() => {
            result.current.updateAudio({ masterVolume: 0 });
        });

        expect(result.current.isSoundEnabled).toBe(false);
    });

    it("should update display settings", () => {
        const { result } = renderHook(() => useGameSettings());

        act(() => {
            result.current.updateDisplay({
                theme: "dark",
                animations: false,
            });
        });

        expect(result.current.settings.display.theme).toBe("dark");
        expect(result.current.settings.display.animations).toBe(false);
        expect(result.current.settings.display.showValidMoves).toBe(true); // unchanged
        expect(result.current.currentTheme).toBe("dark");
    });

    it("should update entire settings", () => {
        const { result } = renderHook(() => useGameSettings());

        const newSettings = {
            timeControl: {
                enabled: true,
                mainTimeMinutes: 60 as const,
                byoyomiSeconds: 10 as const,
            },
            audio: {
                masterVolume: 25 as const,
                pieceSound: false,
                checkSound: false,
                gameEndSound: false,
            },
        };

        act(() => {
            result.current.updateSettings(newSettings);
        });

        expect(result.current.settings.timeControl).toEqual(newSettings.timeControl);
        expect(result.current.settings.audio).toEqual(newSettings.audio);
        // display settings should remain unchanged
        expect(result.current.settings.display).toEqual(defaultGameSettings.display);
    });

    it("should reset settings to defaults", () => {
        const { result } = renderHook(() => useGameSettings());

        // Change some settings first
        act(() => {
            result.current.updateTimeControl({ enabled: true, mainTimeMinutes: 60 });
        });

        act(() => {
            result.current.updateAudio({ masterVolume: 0 });
        });

        act(() => {
            result.current.updateDisplay({ theme: "dark" });
        });

        // Verify settings were changed
        expect(result.current.settings.timeControl.enabled).toBe(true);
        expect(result.current.settings.audio.masterVolume).toBe(0);
        expect(result.current.settings.display.theme).toBe("dark");

        // Reset settings
        act(() => {
            result.current.resetSettings();
        });

        expect(result.current.settings).toEqual(defaultGameSettings);
    });

    it("should validate and fix invalid settings", () => {
        const { result } = renderHook(() => useGameSettings());

        const invalidSettings = {
            audio: {
                masterVolume: 150 as const, // Invalid: > 100
            },
            timeControl: {
                mainTimeMinutes: undefined as never,
            },
        };

        const validatedSettings = result.current.validateAndFixSettings(invalidSettings);

        expect(validatedSettings.audio.masterVolume).toBe(defaultGameSettings.audio.masterVolume);
        expect(validatedSettings.timeControl.mainTimeMinutes).toBe(
            defaultGameSettings.timeControl.mainTimeMinutes,
        );
        expect(validatedSettings.timeControl.enabled).toBe(defaultGameSettings.timeControl.enabled);
    });

    it("should handle negative volume values", () => {
        const { result } = renderHook(() => useGameSettings());

        const invalidSettings = {
            audio: {
                masterVolume: -10 as const,
            },
        };

        const validatedSettings = result.current.validateAndFixSettings(invalidSettings);

        expect(validatedSettings.audio.masterVolume).toBe(defaultGameSettings.audio.masterVolume);
    });

    it("should persist settings to localStorage", () => {
        const { result } = renderHook(() => useGameSettings());

        act(() => {
            result.current.updateTimeControl({ enabled: true });
        });

        expect(mockLocalStorage.setItem).toHaveBeenCalledWith(
            "shogi-game-settings",
            expect.stringContaining('"enabled":true'),
        );
    });

    it("should load settings from localStorage", () => {
        const storedSettings = {
            ...defaultGameSettings,
            timeControl: { ...defaultGameSettings.timeControl, enabled: true },
        };

        mockLocalStorage.setItem("shogi-game-settings", JSON.stringify(storedSettings));

        const { result } = renderHook(() => useGameSettings());

        expect(result.current.settings.timeControl.enabled).toBe(true);
        expect(result.current.isTimeControlEnabled).toBe(true);
    });
});
