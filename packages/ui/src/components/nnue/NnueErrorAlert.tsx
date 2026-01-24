import { getNnueErrorMessage, NnueError } from "@shogi/app-core";
import type { ReactElement } from "react";
import { Button } from "../button";

export interface NnueErrorAlertProps {
    /** エラー */
    error: NnueError | Error | null;
    /** 閉じるボタン押下時のコールバック */
    onClose?: () => void;
}

/**
 * NNUE エラー表示アラート
 */
export function NnueErrorAlert({ error, onClose }: NnueErrorAlertProps): ReactElement | null {
    if (!error) return null;

    const message = error instanceof NnueError ? getNnueErrorMessage(error) : error.message;

    return (
        <div
            role="alert"
            style={{
                display: "flex",
                alignItems: "flex-start",
                gap: "12px",
                padding: "12px 16px",
                borderRadius: "8px",
                backgroundColor: "hsl(var(--destructive, 0 84% 60%) / 0.1)",
                border: "1px solid hsl(var(--destructive, 0 84% 60%) / 0.3)",
                color: "hsl(var(--destructive, 0 84% 60%))",
            }}
        >
            {/* Error icon */}
            <svg
                width="20"
                height="20"
                viewBox="0 0 20 20"
                fill="none"
                style={{ flexShrink: 0, marginTop: "2px" }}
                aria-hidden="true"
            >
                <circle cx="10" cy="10" r="9" stroke="currentColor" strokeWidth="2" />
                <path d="M10 6v5" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
                <circle cx="10" cy="14" r="1" fill="currentColor" />
            </svg>

            {/* Message */}
            <div style={{ flex: 1, fontSize: "14px" }}>{message}</div>

            {/* Close button */}
            {onClose && (
                <Button
                    variant="ghost"
                    size="sm"
                    onClick={onClose}
                    style={{
                        flexShrink: 0,
                        padding: "4px",
                        minWidth: "auto",
                        color: "inherit",
                    }}
                    aria-label="エラーを閉じる"
                >
                    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                        <path
                            d="M4 4l8 8M12 4l-8 8"
                            stroke="currentColor"
                            strokeWidth="2"
                            strokeLinecap="round"
                        />
                    </svg>
                </Button>
            )}
        </div>
    );
}
