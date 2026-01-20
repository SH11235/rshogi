import type { ReactElement } from "react";
import { Button } from "../../button";
import type { GameMode } from "../types";
import { PausedModeControls, PlayingModeControls } from "./GameModeControls";

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
    /** 検討モード開始ボタンのクリックハンドラ */
    onStartReview?: () => void;
    /** 局面編集モードに戻るボタンのクリックハンドラ */
    onEnterEditMode?: () => void;
    /** 投了ボタンのクリックハンドラ */
    onResign?: () => void;
    /** 待ったボタンのクリックハンドラ */
    onUndo?: () => void;
    /** 待った可能かどうか（手数が1以上） */
    canUndo?: boolean;
    /** 対局中かどうか */
    isMatchRunning: boolean;
    /** 現在のゲームモード */
    gameMode?: GameMode;
    /** メッセージ */
    message: string | null;
}

export function MatchControls({
    onResetToStartpos,
    onStop,
    onStart,
    onStartReview,
    onEnterEditMode,
    onResign,
    onUndo,
    canUndo = false,
    isMatchRunning,
    gameMode = "editing",
    message,
}: MatchControlsProps): ReactElement {
    const isEditMode = gameMode === "editing";
    const isReviewMode = gameMode === "reviewing";
    const isPausedMode = gameMode === "paused";

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
                    minHeight: "36px",
                }}
            >
                {/* 対局中: 停止・投了・待ったボタン */}
                {isMatchRunning ? (
                    <PlayingModeControls
                        onStop={onStop}
                        onResign={onResign}
                        onUndo={onUndo}
                        canUndo={canUndo}
                    />
                ) : (
                    <>
                        {/* 編集モード時: [平手に戻す] [検討開始] [対局開始] */}
                        {isEditMode && (
                            <>
                                <Button type="button" onClick={onResetToStartpos} variant="outline">
                                    平手に戻す
                                </Button>
                                {onStartReview && (
                                    <Button
                                        type="button"
                                        onClick={onStartReview}
                                        variant="secondary"
                                    >
                                        検討開始
                                    </Button>
                                )}
                                <Button type="button" onClick={onStart}>
                                    対局開始
                                </Button>
                            </>
                        )}

                        {/* 検討モード時: [平手に戻す] [局面編集] [対局開始] */}
                        {isReviewMode && (
                            <>
                                <Button type="button" onClick={onResetToStartpos} variant="outline">
                                    平手に戻す
                                </Button>
                                {onEnterEditMode && (
                                    <Button
                                        type="button"
                                        onClick={onEnterEditMode}
                                        variant="outline"
                                    >
                                        局面編集
                                    </Button>
                                )}
                                <Button type="button" onClick={onStart}>
                                    対局開始
                                </Button>
                            </>
                        )}

                        {/* 一時停止モード時: [対局再開] [局面編集] [投了] */}
                        {isPausedMode && (
                            <PausedModeControls
                                onResume={onStart}
                                onEnterEditMode={onEnterEditMode}
                                onResign={onResign}
                            />
                        )}
                    </>
                )}
            </div>

            {/* モード表示 */}
            {isEditMode && (
                <div
                    style={{
                        fontSize: "12px",
                        color: "hsl(var(--muted-foreground, 0 0% 48%))",
                        padding: "4px 8px",
                        background: "hsl(var(--muted, 0 0% 96%))",
                        borderRadius: "4px",
                    }}
                >
                    編集モード: 駒をドラッグして盤面を編集できます
                </div>
            )}
            {isReviewMode && (
                <div
                    style={{
                        fontSize: "12px",
                        color: "hsl(var(--muted-foreground, 0 0% 48%))",
                        padding: "4px 8px",
                        background: "hsl(var(--muted, 0 0% 96%))",
                        borderRadius: "4px",
                    }}
                >
                    検討モード: 駒を動かして分岐を作成できます
                </div>
            )}

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
