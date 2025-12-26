import type { Player } from "@shogi/app-core";
import type { ReactElement } from "react";

const baseCard = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
    width: "var(--panel-width)",
};

interface EngineErrorDetails {
    hasError: boolean;
    errorCode?: string;
    errorMessage?: string;
    canRetry: boolean;
}

interface EngineLogsPanelProps {
    /** イベントログのリスト */
    eventLogs: string[];
    /** エラーログのリスト */
    errorLogs: string[];
    /** エンジンエラーの詳細情報 */
    engineErrorDetails?: Record<Player, EngineErrorDetails | null>;
    /** リトライコールバック */
    onRetry?: (side: Player) => void;
}

export function EngineLogsPanel({
    eventLogs,
    errorLogs,
    engineErrorDetails,
    onRetry,
}: EngineLogsPanelProps): ReactElement {
    const hasActiveError =
        engineErrorDetails?.sente?.hasError || engineErrorDetails?.gote?.hasError;

    return (
        <div
            style={{
                ...baseCard,
                border: hasActiveError
                    ? "2px solid hsl(var(--destructive, 0 72% 51%))"
                    : "1px solid hsl(var(--border, 0 0% 86%))",
            }}
        >
            <div style={{ fontWeight: 700, marginBottom: "6px" }}>エンジンログ</div>

            {hasActiveError && (
                <div
                    style={{
                        background: "hsl(var(--destructive, 0 72% 51%) / 0.1)",
                        border: "1px solid hsl(var(--destructive, 0 72% 51%))",
                        borderRadius: "8px",
                        padding: "12px",
                        marginBottom: "8px",
                    }}
                >
                    <div
                        style={{
                            fontWeight: 600,
                            color: "hsl(var(--destructive, 0 72% 51%))",
                            marginBottom: "8px",
                        }}
                    >
                        エンジン初期化エラー
                    </div>

                    {(["sente", "gote"] as const).map((side) => {
                        const error = engineErrorDetails?.[side];
                        if (!error?.hasError) return null;
                        return (
                            <div key={side} style={{ marginBottom: "8px" }}>
                                <div style={{ fontSize: "13px", marginBottom: "4px" }}>
                                    {side === "sente" ? "先手" : "後手"}: {error.errorMessage}
                                </div>
                                {error.canRetry && onRetry && (
                                    <button
                                        type="button"
                                        onClick={() => onRetry(side)}
                                        style={{
                                            padding: "6px 12px",
                                            borderRadius: "6px",
                                            background: "hsl(var(--primary, 15 86% 55%))",
                                            color: "white",
                                            border: "none",
                                            cursor: "pointer",
                                            fontSize: "12px",
                                        }}
                                    >
                                        リトライ
                                    </button>
                                )}
                            </div>
                        );
                    })}
                </div>
            )}

            <ul
                style={{
                    listStyle: "none",
                    padding: 0,
                    margin: 0,
                    display: "flex",
                    flexDirection: "column",
                    gap: "4px",
                    maxHeight: "160px",
                    overflow: "auto",
                }}
            >
                {eventLogs.map((log, idx) => (
                    <li
                        key={`${idx}-${log}`}
                        style={{
                            fontFamily: "ui-monospace, monospace",
                            fontSize: "12px",
                        }}
                    >
                        {log}
                    </li>
                ))}
            </ul>
            {errorLogs.length ? (
                <div
                    style={{
                        marginTop: "8px",
                        color: "hsl(var(--destructive, 0 72% 51%))",
                        fontSize: "12px",
                    }}
                >
                    {errorLogs[0]}
                </div>
            ) : null}
        </div>
    );
}
