import type { GameResult } from "@shogi/app-core";
import { getReasonText, getWinnerLabel } from "@shogi/app-core";
import type { ReactElement } from "react";

interface GameResultBannerProps {
    result: GameResult | null;
    visible: boolean;
    onShowDetail: () => void;
    onClose: () => void;
}

export function GameResultBanner({
    result,
    visible,
    onShowDetail,
    onClose,
}: GameResultBannerProps): ReactElement | null {
    if (!result || !visible) {
        return null;
    }

    const winnerLabel = getWinnerLabel(result.winner);
    const reasonText = getReasonText(result.reason);

    return (
        <div
            style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                gap: "12px",
                padding: "10px 16px",
                backgroundColor: "hsl(var(--card, 0 0% 100%))",
                border: "1px solid hsl(var(--border, 0 0% 86%))",
                borderRadius: "8px",
                boxShadow: "0 2px 8px rgba(0, 0, 0, 0.1)",
                marginBottom: "12px",
            }}
        >
            <span
                style={{
                    fontWeight: "bold",
                    color: "hsl(var(--wafuu-kin, 42 85% 50%))",
                }}
            >
                {winnerLabel}
            </span>

            <span
                style={{
                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                }}
            >
                ({reasonText})
            </span>

            <span
                style={{
                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                    fontSize: "0.875rem",
                }}
            >
                {result.totalMoves}手
            </span>

            <button
                type="button"
                onClick={onShowDetail}
                style={{
                    marginLeft: "8px",
                    padding: "4px 12px",
                    fontSize: "0.875rem",
                    backgroundColor: "hsl(var(--muted, 0 0% 95%))",
                    border: "1px solid hsl(var(--border, 0 0% 86%))",
                    borderRadius: "4px",
                    cursor: "pointer",
                    color: "hsl(var(--foreground, 0 0% 10%))",
                }}
            >
                詳細
            </button>

            <button
                type="button"
                onClick={onClose}
                aria-label="閉じる"
                style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    width: "24px",
                    height: "24px",
                    padding: 0,
                    fontSize: "1rem",
                    backgroundColor: "transparent",
                    border: "none",
                    borderRadius: "4px",
                    cursor: "pointer",
                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                }}
            >
                ×
            </button>
        </div>
    );
}
