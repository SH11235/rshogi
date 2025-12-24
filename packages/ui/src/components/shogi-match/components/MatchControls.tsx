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
    /** 新規対局ボタンのクリックハンドラ */
    onNewGame: () => void;
    /** 停止ボタンのクリックハンドラ */
    onPause: () => void;
    /** 対局開始/再開ボタンのクリックハンドラ */
    onResume: () => void;
    /** メッセージ */
    message: string | null;
}

export function MatchControls({
    onNewGame,
    onPause,
    onResume,
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
                <Button type="button" onClick={onNewGame} style={{ paddingInline: "12px" }}>
                    新規対局（初期化）
                </Button>
                <Button
                    type="button"
                    onClick={onPause}
                    variant="outline"
                    style={{ paddingInline: "12px" }}
                >
                    停止（自動進行オフ）
                </Button>
                <Button
                    type="button"
                    onClick={onResume}
                    variant="secondary"
                    style={{ paddingInline: "12px" }}
                >
                    対局開始 / 再開
                </Button>
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
