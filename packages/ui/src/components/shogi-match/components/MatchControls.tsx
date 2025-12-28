import type { ReactElement } from "react";
import { Button } from "../../button";

const baseCard = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
    width: "var(--panel-width)",
};

interface MatchControlsProps {
    /** 平手に戻すボタンのクリックハンドラ */
    onResetToStartpos: () => void;
    /** 停止ボタンのクリックハンドラ */
    onStop: () => void;
    /** 対局開始ボタンのクリックハンドラ */
    onStart: () => void;
    /** 対局中かどうか */
    isMatchRunning: boolean;
    /** メッセージ */
    message: string | null;
}

export function MatchControls({
    onResetToStartpos,
    onStop,
    onStart,
    isMatchRunning,
    message,
}: MatchControlsProps): ReactElement {
    return (
        <div
            style={{
                ...baseCard,
                display: "flex",
                flexDirection: "column",
                gap: "10px",
            }}
        >
            <div
                style={{
                    display: "flex",
                    gap: "8px",
                    flexWrap: "wrap",
                    alignItems: "center",
                }}
            >
                <Button
                    type="button"
                    onClick={onResetToStartpos}
                    variant="outline"
                    style={{ paddingInline: "12px" }}
                >
                    平手に戻す
                </Button>
                {isMatchRunning ? (
                    <Button
                        type="button"
                        onClick={onStop}
                        variant="destructive"
                        style={{ paddingInline: "16px" }}
                    >
                        停止
                    </Button>
                ) : (
                    <Button type="button" onClick={onStart} style={{ paddingInline: "16px" }}>
                        対局開始
                    </Button>
                )}
            </div>
            {message ? (
                <div
                    style={{
                        color: "hsl(var(--destructive, 0 72% 51%))",
                        fontSize: "13px",
                    }}
                >
                    {message}
                </div>
            ) : null}
        </div>
    );
}
