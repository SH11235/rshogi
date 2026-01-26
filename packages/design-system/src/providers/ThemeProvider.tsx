import {
    createContext,
    type ReactElement,
    type ReactNode,
    useCallback,
    useContext,
    useEffect,
    useMemo,
    useState,
} from "react";

type Theme = "light" | "dark";
type ThemePreference = Theme | "system";

interface ThemeContextValue {
    theme: ThemePreference;
    resolvedTheme: Theme;
    setTheme: (value: ThemePreference) => void;
}

interface ThemeProviderProps {
    children: ReactNode;
    defaultTheme?: ThemePreference;
    storageKey?: string;
}

const DEFAULT_STORAGE_KEY = "shogi-theme";
const PREFERS_DARK_QUERY = "(prefers-color-scheme: dark)";

const ThemeContext = createContext<ThemeContextValue | undefined>(undefined);

function isThemePreference(value: unknown): value is ThemePreference {
    return value === "light" || value === "dark" || value === "system";
}

function readStoredTheme(storageKey: string, fallback: ThemePreference): ThemePreference {
    if (typeof window === "undefined") {
        return fallback;
    }

    const stored = window.localStorage.getItem(storageKey);

    if (isThemePreference(stored)) {
        return stored;
    }

    return fallback;
}

function getSystemTheme(): Theme {
    if (typeof window === "undefined") {
        return "light";
    }

    return window.matchMedia(PREFERS_DARK_QUERY).matches ? "dark" : "light";
}

export function ThemeProvider({
    children,
    defaultTheme = "system",
    storageKey = DEFAULT_STORAGE_KEY,
}: ThemeProviderProps): ReactElement {
    const [theme, setThemePreference] = useState<ThemePreference>(() =>
        readStoredTheme(storageKey, defaultTheme),
    );
    const [systemTheme, setSystemTheme] = useState<Theme>(() =>
        defaultTheme === "dark" ? "dark" : getSystemTheme(),
    );

    useEffect(() => {
        if (typeof window === "undefined") {
            return;
        }

        const mediaQuery = window.matchMedia(PREFERS_DARK_QUERY);
        const updateSystemTheme = (event: MediaQueryListEvent | MediaQueryList): void => {
            const matchesDark = "matches" in event ? event.matches : mediaQuery.matches;
            setSystemTheme(matchesDark ? "dark" : "light");
        };

        updateSystemTheme(mediaQuery);

        if (typeof mediaQuery.addEventListener === "function") {
            mediaQuery.addEventListener("change", updateSystemTheme);
            return () => mediaQuery.removeEventListener("change", updateSystemTheme);
        }

        mediaQuery.addListener(updateSystemTheme);
        return () => mediaQuery.removeListener(updateSystemTheme);
    }, []);

    const resolvedTheme = theme === "system" ? systemTheme : theme;

    useEffect(() => {
        if (typeof document === "undefined") {
            return;
        }

        const root = document.documentElement;
        root.classList.remove("light", "dark");
        root.classList.add(resolvedTheme);
        root.setAttribute("data-theme", resolvedTheme);

        if (typeof window === "undefined") {
            return;
        }

        if (theme === "system") {
            window.localStorage.removeItem(storageKey);
        } else {
            window.localStorage.setItem(storageKey, theme);
        }
    }, [resolvedTheme, storageKey, theme]);

    const setTheme = useCallback((nextTheme: ThemePreference) => {
        setThemePreference(nextTheme);
    }, []);

    const value = useMemo<ThemeContextValue>(
        () => ({
            theme,
            resolvedTheme,
            setTheme,
        }),
        [resolvedTheme, setTheme, theme],
    );

    return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme(): ThemeContextValue {
    const context = useContext(ThemeContext);

    if (!context) {
        throw new Error("useTheme must be used within a ThemeProvider");
    }

    return context;
}
