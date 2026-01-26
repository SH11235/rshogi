import type { ReactElement } from "react";
import { Spinner } from "../spinner";

interface EngineRestartingOverlayProps {
    /** 表示するかどうか */
    visible: boolean;
}

/**
 * エンジン再起動中オーバーレイ
 */
export function EngineRestartingOverlay({
    visible,
}: EngineRestartingOverlayProps): ReactElement | null {
    if (!visible) return null;

    return (
        <div
            style={{
                position: "fixed",
                inset: 0,
                backgroundColor: "rgba(0, 0, 0, 0.5)",
                display: "flex",
                flexDirection: "column",
                alignItems: "center",
                justifyContent: "center",
                gap: "16px",
                zIndex: 100,
            }}
        >
            <div
                style={{
                    backgroundColor: "hsl(var(--card, 0 0% 100%))",
                    borderRadius: "12px",
                    padding: "32px 48px",
                    display: "flex",
                    flexDirection: "column",
                    alignItems: "center",
                    gap: "16px",
                    boxShadow: "0 8px 30px rgba(0, 0, 0, 0.2)",
                }}
            >
                <Spinner size="xl" label="エンジン再起動中" />
                <div
                    style={{
                        color: "hsl(var(--foreground, 0 0% 10%))",
                        fontWeight: 500,
                        fontSize: "16px",
                    }}
                >
                    エンジン再起動中...
                </div>
                <div
                    style={{
                        color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        fontSize: "13px",
                    }}
                >
                    しばらくお待ちください
                </div>
            </div>
        </div>
    );
}
