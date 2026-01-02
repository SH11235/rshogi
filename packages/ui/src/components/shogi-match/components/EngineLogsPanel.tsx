import type { Player } from "@shogi/app-core";
import { type EngineErrorCode, getEngineErrorInfo } from "@shogi/engine-client";
import { useState, type ReactElement } from "react";

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
    errorCode?: EngineErrorCode;
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
    /** リトライ中かどうか */
    isRetrying?: Record<Player, boolean>;
}

/** 個別エラー詳細表示コンポーネント */
function ErrorDetailSection({
    side,
    error,
    onRetry,
    isRetrying,
}: {
    side: Player;
    error: EngineErrorDetails;
    onRetry?: (side: Player) => void;
    isRetrying?: boolean;
}): ReactElement | null {
    const [showDetails, setShowDetails] = useState(false);
    const errorInfo = getEngineErrorInfo(error.errorCode);

    return (
        <div
            style={{
                background: "hsl(var(--background, 0 0% 100%))",
                borderRadius: "6px",
                padding: "12px",
                marginBottom: "8px",
            }}
        >
            {/* ヘッダー: 側面とメインメッセージ */}
            <div
                style={{
                    display: "flex",
                    alignItems: "center",
                    gap: "8px",
                    marginBottom: "8px",
                }}
            >
                <span
                    style={{
                        background: side === "sente" ? "#1a1a1a" : "#e0e0e0",
                        color: side === "sente" ? "#fff" : "#1a1a1a",
                        padding: "2px 8px",
                        borderRadius: "4px",
                        fontSize: "11px",
                        fontWeight: 600,
                    }}
                >
                    {side === "sente" ? "先手" : "後手"}
                </span>
                <span style={{ fontWeight: 600, fontSize: "14px" }}>{errorInfo.userMessage}</span>
            </div>

            {/* 考えられる原因 */}
            <div style={{ marginBottom: "8px" }}>
                <div
                    style={{
                        fontSize: "12px",
                        color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        marginBottom: "4px",
                    }}
                >
                    考えられる原因:
                </div>
                <ul style={{ margin: 0, paddingLeft: "16px", fontSize: "12px" }}>
                    {errorInfo.possibleCauses.map((cause, i) => (
                        <li key={i} style={{ marginBottom: "2px" }}>
                            {cause}
                        </li>
                    ))}
                </ul>
            </div>

            {/* 対処法 */}
            <div style={{ marginBottom: "12px" }}>
                <div
                    style={{
                        fontSize: "12px",
                        color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        marginBottom: "4px",
                    }}
                >
                    対処法:
                </div>
                <ul style={{ margin: 0, paddingLeft: "16px", fontSize: "12px" }}>
                    {errorInfo.solutions.map((solution, i) => (
                        <li key={i} style={{ marginBottom: "2px" }}>
                            {solution}
                        </li>
                    ))}
                </ul>
            </div>

            {/* アクションボタン */}
            <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
                {errorInfo.canRetry && onRetry && (
                    <button
                        type="button"
                        onClick={() => onRetry(side)}
                        disabled={isRetrying}
                        style={{
                            padding: "8px 16px",
                            borderRadius: "6px",
                            background: isRetrying
                                ? "hsl(var(--muted, 0 0% 80%))"
                                : "hsl(var(--primary, 15 86% 55%))",
                            color: "white",
                            border: "none",
                            cursor: isRetrying ? "not-allowed" : "pointer",
                            fontSize: "13px",
                            fontWeight: 500,
                            opacity: isRetrying ? 0.6 : 1,
                        }}
                    >
                        {isRetrying ? "リトライ中..." : "再試行"}
                    </button>
                )}
                <button
                    type="button"
                    onClick={() => setShowDetails(!showDetails)}
                    style={{
                        padding: "8px 12px",
                        borderRadius: "6px",
                        background: "transparent",
                        color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        border: "1px solid hsl(var(--border, 0 0% 86%))",
                        cursor: "pointer",
                        fontSize: "12px",
                    }}
                >
                    {showDetails ? "詳細を隠す" : "詳細を表示"}
                </button>
            </div>

            {/* 技術的な詳細（折りたたみ） */}
            {showDetails && (
                <div
                    style={{
                        marginTop: "12px",
                        padding: "8px",
                        background: "hsl(var(--muted, 0 0% 96%))",
                        borderRadius: "4px",
                        fontSize: "11px",
                        fontFamily: "ui-monospace, monospace",
                    }}
                >
                    <div>エラーコード: {error.errorCode ?? "UNKNOWN"}</div>
                    {error.errorMessage && <div>メッセージ: {error.errorMessage}</div>}
                </div>
            )}
        </div>
    );
}

export function EngineLogsPanel({
    eventLogs,
    errorLogs,
    engineErrorDetails,
    onRetry,
    isRetrying,
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
                            display: "flex",
                            alignItems: "center",
                            gap: "8px",
                            fontWeight: 600,
                            color: "hsl(var(--destructive, 0 72% 51%))",
                            marginBottom: "12px",
                            fontSize: "15px",
                        }}
                    >
                        <span style={{ fontSize: "18px" }}>⚠️</span>
                        エンジンエラー
                    </div>

                    {(["sente", "gote"] as const).map((side) => {
                        const error = engineErrorDetails?.[side];
                        if (!error?.hasError) return null;
                        return (
                            <ErrorDetailSection
                                key={side}
                                side={side}
                                error={error}
                                onRetry={onRetry}
                                isRetrying={isRetrying?.[side]}
                            />
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
