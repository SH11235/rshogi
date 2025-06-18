import type { KnipConfig } from "knip";

const config: KnipConfig = {
    /**
     * エントリーポイントの設定
     */
    entry: [
        "src/main.tsx",
        "src/App.tsx",
        "src/**/*.stories.tsx", // Storybook files
        "src/**/*.test.ts", // Test files
        "src/**/*.test.tsx", // Test files
        "vitest.config.ts",
        "vite.config.ts",
        ".storybook/**/*.ts",
    ],

    /**
     * プロジェクトファイルのパターン
     */
    project: ["src/**/*.{ts,tsx}", "!src/**/*.test.{ts,tsx}", "!src/**/*.stories.{ts,tsx}"],

    /**
     * 無視するファイル（意図的に未使用だが残したいもの）
     */
    ignore: [
        // 将来使用予定のファイル
        "src/types/audioConfig.ts",
        "src/types/audioErrors.ts",
        "src/types/result.ts",
        "src/types/timer.ts",

        // UI component library (shadcn/ui)
        "src/types/ui.ts",
        "src/components/ui/alert-dialog.tsx",
        "src/components/ui/card.tsx",

        // Audio development utilities
        "src/services/audioGenerator.ts",
        "src/services/audioLogger.ts",
        "src/services/audioManager.ts",

        // Timer system (将来使用予定)
        "src/hooks/useGameTimer.ts",
        "src/stores/timerStore.ts",

        // Storybook設定ファイル
        ".storybook/preview.ts",
    ],

    /**
     * 無視する依存関係
     */
    ignoreDependencies: [
        // Storybook関連（開発時のみ使用）
        "@storybook/react",

        // ビルドツール（package.jsonから参照）
        "@biomejs/biome",
        "tailwindcss",
        "storybook",

        // Core library （間接的に使用）
        "shogi-core",
    ],

    /**
     * 無視するバイナリ
     */
    ignoreBinaries: ["vite", "tsc", "biome", "vitest", "playwright", "storybook", "knip"],

    /**
     * 使用されていないエクスポートで無視するもの
     */
    ignoreExportsUsedInFile: {
        // UI component のインターフェース（外部利用想定）
        interface: true,
        type: true,
    },

    /**
     * Vite設定
     */
    vite: {
        config: "vite.config.ts",
    },

    /**
     * Vitest設定
     */
    vitest: {
        config: "vitest.config.ts",
    },

    /**
     * Storybook設定
     */
    storybook: {
        config: ".storybook/main.ts",
        entry: ["src/**/*.stories.@(js|jsx|ts|tsx|mdx)"],
    },

    /**
     * TypeScript設定
     */
    typescript: {
        config: "tsconfig.json",
    },

    /**
     * 追加のワークスペース（必要に応じて）
     */
    workspaces: {
        ".": {
            entry: "src/main.tsx",
        },
    },
};

export default config;
