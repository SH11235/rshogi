import type { ReactElement } from "react";

const baseCard = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
};

interface EngineLogsPanelProps {
    /** イベントログのリスト */
    eventLogs: string[];
    /** エラーログのリスト */
    errorLogs: string[];
}

export function EngineLogsPanel({ eventLogs, errorLogs }: EngineLogsPanelProps): ReactElement {
    return (
        <div style={baseCard}>
            <div style={{ fontWeight: 700, marginBottom: "6px" }}>エンジンログ</div>
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
