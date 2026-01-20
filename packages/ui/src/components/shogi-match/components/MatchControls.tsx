import type { ReactElement } from "react";
import { Button } from "../../button";
import type { GameMode } from "../types";
import { PausedModeControls, PlayingModeControls } from "./GameModeControls";

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
        <div className="flex flex-col gap-2 items-center">
            {/* ボタン行 */}
            <div className="flex gap-2 flex-wrap justify-center min-h-[36px] items-center">
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
                        {/* 編集モード時: [対局開始] [平手に戻す] [検討開始] */}
                        {isEditMode && (
                            <>
                                <Button type="button" onClick={onStart}>
                                    対局開始
                                </Button>
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
                            </>
                        )}

                        {/* 検討モード時: [対局開始] [平手に戻す] [局面編集] */}
                        {isReviewMode && (
                            <>
                                <Button type="button" onClick={onStart}>
                                    対局開始
                                </Button>
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
                <div className="text-xs text-muted-foreground">
                    編集モード: 駒をドラッグして盤面を編集できます
                </div>
            )}
            {isReviewMode && (
                <div className="text-xs text-muted-foreground">
                    検討モード: 駒を動かして分岐を作成できます
                </div>
            )}

            {/* エラーメッセージ */}
            {message && <div className="text-destructive text-sm">{message}</div>}
        </div>
    );
}
