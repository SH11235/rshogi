import type { ReactElement } from "react";
import { Progress } from "../progress";
import { Spinner } from "../spinner";

export interface NnueProgressOverlayProps {
    /** 表示するかどうか */
    visible: boolean;
    /** 進捗値 (0-100)。undefined の場合は不確定モード */
    progress?: number;
    /** メッセージ */
    message?: string;
}

/**
 * NNUE インポート/ロード進捗オーバーレイ
 */
export function NnueProgressOverlay({
    visible,
    progress,
    message = "処理中...",
}: NnueProgressOverlayProps): ReactElement | null {
    if (!visible) return null;

    return (
        <div
            style={{
                position: "absolute",
                inset: 0,
                backgroundColor: "rgba(255, 255, 255, 0.9)",
                display: "flex",
                flexDirection: "column",
                alignItems: "center",
                justifyContent: "center",
                gap: "16px",
                borderRadius: "8px",
                zIndex: 10,
            }}
        >
            <Spinner size="lg" label={message} />
            <div
                style={{
                    color: "hsl(var(--foreground, 0 0% 10%))",
                    fontWeight: 500,
                }}
            >
                {message}
            </div>
            {progress !== undefined && (
                <div style={{ width: "200px" }}>
                    <Progress value={progress} />
                    <div
                        style={{
                            textAlign: "center",
                            marginTop: "8px",
                            fontSize: "13px",
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        }}
                    >
                        {Math.round(progress)}%
                    </div>
                </div>
            )}
        </div>
    );
}
