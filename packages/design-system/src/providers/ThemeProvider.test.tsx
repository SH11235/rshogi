import { render, screen } from "@testing-library/react";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ThemeProvider, useTheme } from "./ThemeProvider";

describe("ThemeProvider", () => {
    let localStorageGetSpy: ReturnType<typeof vi.spyOn>;
    let localStorageSetSpy: ReturnType<typeof vi.spyOn>;
    let localStorageRemoveSpy: ReturnType<typeof vi.spyOn>;
    let matchMediaMock: ReturnType<typeof vi.fn>;

    beforeEach(() => {
        // localStorage のスパイ
        localStorageGetSpy = vi.spyOn(window.localStorage, "getItem");
        localStorageSetSpy = vi.spyOn(window.localStorage, "setItem");
        localStorageRemoveSpy = vi.spyOn(window.localStorage, "removeItem");

        // matchMedia のモック
        matchMediaMock = vi.fn().mockImplementation((query: string) => ({
            matches: false,
            media: query,
            addEventListener: vi.fn(),
            removeEventListener: vi.fn(),
            addListener: vi.fn(),
            removeListener: vi.fn(),
        }));
        window.matchMedia = matchMediaMock;
    });

    afterEach(() => {
        vi.restoreAllMocks();
        localStorage.clear();
    });

    describe("基本動作", () => {
        it("子要素を正しくレンダリングする", () => {
            render(
                <ThemeProvider>
                    <div>Test Content</div>
                </ThemeProvider>,
            );

            expect(screen.getByText("Test Content")).toBeDefined();
        });

        it("デフォルトで system テーマを使用する", () => {
            function TestComponent() {
                const { theme } = useTheme();
                return <div>Theme: {theme}</div>;
            }

            render(
                <ThemeProvider>
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Theme: system")).toBeDefined();
        });

        it("defaultTheme プロパティでデフォルトテーマを設定できる", () => {
            function TestComponent() {
                const { theme } = useTheme();
                return <div>Theme: {theme}</div>;
            }

            render(
                <ThemeProvider defaultTheme="dark">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Theme: dark")).toBeDefined();
        });

        it("setTheme でテーマを切り替える", () => {
            function TestComponent() {
                const { theme, setTheme } = useTheme();
                return (
                    <div>
                        <div>Current: {theme}</div>
                        <button type="button" onClick={() => setTheme("dark")}>
                            Set Dark
                        </button>
                    </div>
                );
            }

            render(
                <ThemeProvider>
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Current: system")).toBeDefined();

            const button = screen.getByRole("button");
            act(() => {
                button.click();
            });

            expect(screen.getByText("Current: dark")).toBeDefined();
        });
    });

    describe("localStorage 連携", () => {
        it("テーマを localStorage に保存する", () => {
            function TestComponent() {
                const { setTheme } = useTheme();
                return (
                    <button type="button" onClick={() => setTheme("dark")}>
                        Set Dark
                    </button>
                );
            }

            render(
                <ThemeProvider>
                    <TestComponent />
                </ThemeProvider>,
            );

            const button = screen.getByRole("button");
            act(() => {
                button.click();
            });

            expect(localStorageSetSpy).toHaveBeenCalledWith("shogi-theme", "dark");
        });

        it("localStorage からテーマを復元する", () => {
            localStorageGetSpy.mockReturnValue("dark");

            function TestComponent() {
                const { theme } = useTheme();
                return <div>Theme: {theme}</div>;
            }

            render(
                <ThemeProvider>
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Theme: dark")).toBeDefined();
            expect(localStorageGetSpy).toHaveBeenCalledWith("shogi-theme");
        });

        it("カスタム storageKey を使用できる", () => {
            localStorageGetSpy.mockReturnValue("light");

            function TestComponent() {
                const { theme } = useTheme();
                return <div>Theme: {theme}</div>;
            }

            render(
                <ThemeProvider storageKey="custom-theme-key">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(localStorageGetSpy).toHaveBeenCalledWith("custom-theme-key");
        });

        it("system テーマの場合は localStorage から削除する", () => {
            function TestComponent() {
                const { setTheme } = useTheme();
                return (
                    <button type="button" onClick={() => setTheme("system")}>
                        Set System
                    </button>
                );
            }

            render(
                <ThemeProvider defaultTheme="dark">
                    <TestComponent />
                </ThemeProvider>,
            );

            const button = screen.getByRole("button");
            act(() => {
                button.click();
            });

            expect(localStorageRemoveSpy).toHaveBeenCalledWith("shogi-theme");
        });

        it("無効な localStorage 値の場合はデフォルトを使用する", () => {
            localStorageGetSpy.mockReturnValue("invalid-theme");

            function TestComponent() {
                const { theme } = useTheme();
                return <div>Theme: {theme}</div>;
            }

            render(
                <ThemeProvider defaultTheme="light">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Theme: light")).toBeDefined();
        });
    });

    describe("system テーマ", () => {
        it("system テーマでシステムのダークモードを検出する", () => {
            matchMediaMock.mockImplementation((query: string) => ({
                matches: true, // ダークモード
                media: query,
                addEventListener: vi.fn(),
                removeEventListener: vi.fn(),
                addListener: vi.fn(),
                removeListener: vi.fn(),
            }));

            function TestComponent() {
                const { resolvedTheme } = useTheme();
                return <div>Resolved: {resolvedTheme}</div>;
            }

            render(
                <ThemeProvider defaultTheme="system">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Resolved: dark")).toBeDefined();
        });

        it("system テーマでシステムのライトモードを検出する", () => {
            matchMediaMock.mockImplementation((query: string) => ({
                matches: false, // ライトモード
                media: query,
                addEventListener: vi.fn(),
                removeEventListener: vi.fn(),
                addListener: vi.fn(),
                removeListener: vi.fn(),
            }));

            function TestComponent() {
                const { resolvedTheme } = useTheme();
                return <div>Resolved: {resolvedTheme}</div>;
            }

            render(
                <ThemeProvider defaultTheme="system">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Resolved: light")).toBeDefined();
        });

        it("システムのテーマ変更を検出する", () => {
            let changeHandler: ((event: MediaQueryListEvent) => void) | null = null;

            matchMediaMock.mockImplementation((query: string) => ({
                matches: false,
                media: query,
                addEventListener: vi.fn((event, handler) => {
                    if (event === "change") {
                        changeHandler = handler;
                    }
                }),
                removeEventListener: vi.fn(),
                addListener: vi.fn(),
                removeListener: vi.fn(),
            }));

            function TestComponent() {
                const { resolvedTheme } = useTheme();
                return <div>Resolved: {resolvedTheme}</div>;
            }

            render(
                <ThemeProvider defaultTheme="system">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Resolved: light")).toBeDefined();

            // システムのテーマをダークに変更
            act(() => {
                changeHandler?.({ matches: true } as MediaQueryListEvent);
            });

            expect(screen.getByText("Resolved: dark")).toBeDefined();
        });
    });

    describe("resolvedTheme", () => {
        it("light テーマの場合は light を返す", () => {
            function TestComponent() {
                const { theme, resolvedTheme } = useTheme();
                return (
                    <div>
                        Theme: {theme}, Resolved: {resolvedTheme}
                    </div>
                );
            }

            render(
                <ThemeProvider defaultTheme="light">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Theme: light, Resolved: light")).toBeDefined();
        });

        it("dark テーマの場合は dark を返す", () => {
            function TestComponent() {
                const { theme, resolvedTheme } = useTheme();
                return (
                    <div>
                        Theme: {theme}, Resolved: {resolvedTheme}
                    </div>
                );
            }

            render(
                <ThemeProvider defaultTheme="dark">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Theme: dark, Resolved: dark")).toBeDefined();
        });

        it("system テーマの場合はシステムテーマを返す", () => {
            matchMediaMock.mockImplementation((query: string) => ({
                matches: true,
                media: query,
                addEventListener: vi.fn(),
                removeEventListener: vi.fn(),
                addListener: vi.fn(),
                removeListener: vi.fn(),
            }));

            function TestComponent() {
                const { theme, resolvedTheme } = useTheme();
                return (
                    <div>
                        Theme: {theme}, Resolved: {resolvedTheme}
                    </div>
                );
            }

            render(
                <ThemeProvider defaultTheme="system">
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Theme: system, Resolved: dark")).toBeDefined();
        });
    });

    describe("DOM 操作", () => {
        it("resolvedTheme に応じて documentElement のクラスを更新する", () => {
            function TestComponent() {
                const { setTheme } = useTheme();
                return (
                    <button type="button" onClick={() => setTheme("dark")}>
                        Set Dark
                    </button>
                );
            }

            render(
                <ThemeProvider defaultTheme="light">
                    <TestComponent />
                </ThemeProvider>,
            );

            // 初期状態
            expect(document.documentElement.classList.contains("light")).toBe(true);
            expect(document.documentElement.getAttribute("data-theme")).toBe("light");

            // テーマを変更
            const button = screen.getByRole("button");
            act(() => {
                button.click();
            });

            expect(document.documentElement.classList.contains("dark")).toBe(true);
            expect(document.documentElement.classList.contains("light")).toBe(false);
            expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
        });
    });

    describe("useTheme フック", () => {
        it("Provider 外で使用するとエラーを投げる", () => {
            function TestComponent() {
                useTheme();
                return <div>Test</div>;
            }

            // エラーログを抑制
            const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

            expect(() => {
                render(<TestComponent />);
            }).toThrow("useTheme must be used within a ThemeProvider");

            consoleError.mockRestore();
        });

        it("theme, resolvedTheme, setTheme を返す", () => {
            function TestComponent() {
                const context = useTheme();
                return (
                    <div>
                        <div>Has theme: {typeof context.theme === "string" ? "yes" : "no"}</div>
                        <div>
                            Has resolvedTheme:{" "}
                            {typeof context.resolvedTheme === "string" ? "yes" : "no"}
                        </div>
                        <div>
                            Has setTheme: {typeof context.setTheme === "function" ? "yes" : "no"}
                        </div>
                    </div>
                );
            }

            render(
                <ThemeProvider>
                    <TestComponent />
                </ThemeProvider>,
            );

            expect(screen.getByText("Has theme: yes")).toBeDefined();
            expect(screen.getByText("Has resolvedTheme: yes")).toBeDefined();
            expect(screen.getByText("Has setTheme: yes")).toBeDefined();
        });
    });
});
