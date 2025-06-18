/**
 * 音声システム専用のロガー（function-based）
 * 開発環境でのみ動作し、本番環境では無効化される
 */

export type LogLevel = "debug" | "info" | "warn" | "error";

export interface LogEntry {
    readonly level: LogLevel;
    readonly context: string;
    readonly message: string;
    readonly args: readonly unknown[];
    readonly timestamp: number;
}

/**
 * ログ設定
 */
export interface LoggerConfig {
    readonly isDevelopment: boolean;
    readonly enabledLevels: readonly LogLevel[];
    readonly formatTimestamp?: (timestamp: number) => string;
    readonly formatPrefix?: (context: string, timestamp: string) => string;
}

/**
 * デフォルトのログ設定
 */
const defaultLoggerConfig: LoggerConfig = {
    isDevelopment: import.meta.env.DEV,
    enabledLevels: ["debug", "info", "warn", "error"],
    formatTimestamp: (timestamp: number) =>
        new Date(timestamp).toISOString().split("T")[1].split(".")[0],
    formatPrefix: (context: string, timestamp: string) => `[${timestamp}] 🔊 ${context}:`,
};

/**
 * ログ出力関数
 */
const logToConsole = (entry: LogEntry, config: LoggerConfig): void => {
    if (!config.isDevelopment || !config.enabledLevels.includes(entry.level)) {
        return;
    }

    const timestamp = config.formatTimestamp?.(entry.timestamp) ?? String(entry.timestamp);
    const prefix = config.formatPrefix?.(entry.context, timestamp) ?? `${entry.context}:`;

    switch (entry.level) {
        case "debug":
            console.debug(prefix, entry.message, ...entry.args);
            break;
        case "info":
            console.info(prefix, entry.message, ...entry.args);
            break;
        case "warn":
            console.warn(prefix, entry.message, ...entry.args);
            break;
        case "error":
            console.error(prefix, entry.message, ...entry.args);
            break;
    }
};

/**
 * ログエントリー作成関数
 */
const createLogEntry = (
    level: LogLevel,
    context: string,
    message: string,
    args: readonly unknown[],
): LogEntry => ({
    level,
    context,
    message,
    args,
    timestamp: Date.now(),
});

/**
 * ロガー作成関数
 */
export const createAudioLogger = (config: Partial<LoggerConfig> = {}) => {
    const finalConfig: LoggerConfig = { ...defaultLoggerConfig, ...config };

    const log = (level: LogLevel, context: string, message: string, ...args: unknown[]): void => {
        const entry = createLogEntry(level, context, message, args);
        logToConsole(entry, finalConfig);
    };

    return {
        debug: (context: string, message: string, ...args: unknown[]): void =>
            log("debug", context, message, ...args),

        info: (context: string, message: string, ...args: unknown[]): void =>
            log("info", context, message, ...args),

        warn: (context: string, message: string, ...args: unknown[]): void =>
            log("warn", context, message, ...args),

        error: (context: string, message: string, ...args: unknown[]): void =>
            log("error", context, message, ...args),

        /**
         * 設定の更新（新しいロガーインスタンスを返す）
         */
        withConfig: (newConfig: Partial<LoggerConfig>) =>
            createAudioLogger({ ...finalConfig, ...newConfig }),

        /**
         * 現在の設定を取得
         */
        getConfig: () => finalConfig,
    } as const;
};

/**
 * デフォルトの音声ロガーインスタンス
 */
export const audioLogger = createAudioLogger();
