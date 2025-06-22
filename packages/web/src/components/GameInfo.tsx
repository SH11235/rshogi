import { cn } from "@/lib/utils";
import { useGameStore } from "@/stores/gameStore";
import { useState } from "react";
import type { GameStatus, Move, Player } from "shogi-core";
import { KeyboardHelp } from "./KeyboardHelp";
import { TimerDisplay } from "./TimerDisplay";
import { TimerSettingsDialog } from "./TimerSettingsDialog";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
    AlertDialogTrigger,
} from "./ui/alert-dialog";

interface GameInfoProps {
    currentPlayer: Player;
    gameStatus: GameStatus;
    moveHistory: Move[];
    historyCursor: number;
    resignedPlayer: Player | null;
    isTsumeShogi: boolean;
    gameMode: "playing" | "review" | "analysis";
    hasReviewBase?: boolean;
    onReset: () => void;
    onResign?: () => void;
    onStartFromPosition?: () => void;
    onReturnToReview?: () => void;
}

export function GameInfo({
    currentPlayer,
    gameStatus,
    moveHistory,
    historyCursor,
    resignedPlayer,
    isTsumeShogi,
    gameMode,
    hasReviewBase,
    onReset,
    onResign,
    onStartFromPosition,
    onReturnToReview,
}: GameInfoProps) {
    const moveCount = moveHistory.length;
    // Calculate turn based on current position in history
    // When historyCursor is -1 (latest position), show next move number
    // When historyCursor >= 0, show the position after that move
    const turn =
        historyCursor === -1 || historyCursor === undefined ? moveCount + 1 : historyCursor + 2;
    const [isResetDialogOpen, setIsResetDialogOpen] = useState(false);
    const [isResignDialogOpen, setIsResignDialogOpen] = useState(false);
    const [isTimerSettingsOpen, setIsTimerSettingsOpen] = useState(false);
    const timer = useGameStore((state) => state.timer);
    const pauseTimer = useGameStore((state) => state.pauseTimer);
    const resumeTimer = useGameStore((state) => state.resumeTimer);

    const getStatusMessage = () => {
        // 詰将棋モードの場合の特別な表示
        if (isTsumeShogi) {
            switch (gameStatus) {
                case "checkmate":
                    // 最終手まで進んでいる場合は「詰み」、そうでない場合は通常表示
                    if (historyCursor === moveHistory.length - 1 || historyCursor === -1) {
                        return "詰み！正解です！";
                    }
                    return currentPlayer === "black" ? "先手番" : "後手番";
                case "check":
                    return `王手！ - ${currentPlayer === "black" ? "先手番" : "後手番"}`;
                default:
                    return currentPlayer === "black" ? "先手番" : "後手番";
            }
        }

        // 通常の対局の場合
        switch (gameStatus) {
            case "black_win":
                return "先手の勝ち！";
            case "white_win":
                return "後手の勝ち！";
            case "checkmate":
                return "詰み";
            case "check":
                return `王手！ - ${currentPlayer === "black" ? "先手番" : "後手番"}`;
            case "draw":
                return "引き分け";
            case "sennichite":
                return "千日手";
            case "perpetual_check":
                return "連続王手の千日手";
            case "timeout":
                return "時間切れ";
            case "resigned":
                return "投了";
            default:
                return currentPlayer === "black" ? "先手番" : "後手番";
        }
    };

    // 勝敗理由の詳細メッセージを取得
    const getDetailedMessage = () => {
        switch (gameStatus) {
            case "black_win":
                if (resignedPlayer === "white") {
                    return "後手が投了しました";
                }
                if (timer.hasTimedOut && timer.timedOutPlayer === "white") {
                    return "後手の時間切れ";
                }
                if (moveCount > 0) {
                    return "詰みにより勝利";
                }
                return "";
            case "white_win":
                if (resignedPlayer === "black") {
                    return "先手が投了しました";
                }
                if (timer.hasTimedOut && timer.timedOutPlayer === "black") {
                    return "先手の時間切れ";
                }
                if (moveCount > 0) {
                    return "詰みにより勝利";
                }
                return "";
            case "sennichite":
                return "同一局面が4回現れました";
            case "perpetual_check":
                return "同一手順による連続王手";
            case "timeout":
                return "持ち時間が切れました";
            case "resigned":
                return "投了しました";
            default:
                return "";
        }
    };

    const getStatusColor = () => {
        switch (gameStatus) {
            case "black_win":
            case "white_win":
                return "text-green-600";
            case "checkmate":
            case "check":
                return "text-red-600";
            case "draw":
            case "sennichite":
            case "perpetual_check":
                return "text-yellow-600";
            case "timeout":
            case "resigned":
                return "text-gray-600";
            default:
                return currentPlayer === "black" ? "text-black" : "text-red-600";
        }
    };

    const isGameOver = !["playing", "check"].includes(gameStatus);
    const hasMovesPlayed = moveCount > 0;

    const handleResetClick = () => {
        if (hasMovesPlayed && !isGameOver) {
            setIsResetDialogOpen(true);
        } else {
            onReset();
        }
    };

    const handleConfirmReset = () => {
        onReset();
        setIsResetDialogOpen(false);
    };

    const handleConfirmResign = () => {
        if (onResign) {
            onResign();
        }
        setIsResignDialogOpen(false);
    };

    return (
        <div className="p-3 sm:p-6 bg-white rounded-lg shadow-md">
            {/* モード表示 */}
            <div className="mb-3 text-center flex justify-center gap-2">
                {/* 詰将棋モード */}
                {isTsumeShogi && (
                    <div className="text-sm text-purple-600 font-bold bg-purple-50 px-3 py-1 rounded-full inline-block">
                        🎯 詰将棋
                    </div>
                )}
                {/* ゲームモード */}
                {gameMode === "review" && (
                    <div className="text-sm text-blue-600 font-bold bg-blue-50 px-3 py-1 rounded-full inline-block">
                        👀 閲覧モード
                    </div>
                )}
                {gameMode === "analysis" && (
                    <div className="text-sm text-green-600 font-bold bg-green-50 px-3 py-1 rounded-full inline-block">
                        🔍 解析モード
                    </div>
                )}
            </div>

            {/* タイマー表示 - 対局モードと解析モードのみ */}
            {timer.config.mode && gameMode !== "review" && (
                <div className="mb-4 grid grid-cols-2 gap-4">
                    <div className="text-center">
                        <div className="text-sm text-gray-600 mb-1">先手</div>
                        <TimerDisplay player="black" />
                    </div>
                    <div className="text-center">
                        <div className="text-sm text-gray-600 mb-1">後手</div>
                        <TimerDisplay player="white" />
                    </div>
                </div>
            )}

            {/* ゲーム状態表示 - 固定高さで安定化 */}
            <div className="mb-3 sm:mb-4 text-center min-h-[180px] sm:min-h-[200px] flex flex-col justify-center">
                <h2
                    className={cn(
                        "text-lg sm:text-xl lg:text-2xl font-bold mb-2 transition-all duration-200",
                        getStatusColor(),
                    )}
                >
                    {getStatusMessage()}
                </h2>

                {/* 詳細メッセージ - 常にスペースを確保 */}
                <div className="h-6 sm:h-7 mb-2">
                    <p
                        className={cn(
                            "text-xs sm:text-sm text-gray-600 transition-opacity duration-200",
                            getDetailedMessage() ? "opacity-100" : "opacity-0",
                        )}
                    >
                        {getDetailedMessage() || "　"}
                    </p>
                </div>

                {/* 手数表示 */}
                <div className="flex justify-center gap-2 sm:gap-4 text-xs sm:text-sm text-gray-600 mb-2 sm:mb-3">
                    <span>第{turn}手</span>
                    <span>•</span>
                    <span>総手数: {moveCount}</span>
                </div>

                {/* 追加の状態情報 - 常にスペースを確保 */}
                <div className="h-8 sm:h-9">
                    <div
                        className={cn(
                            "text-red-500 text-xs sm:text-sm font-medium bg-red-50 px-2 sm:px-3 py-1 rounded-full inline-block transition-all duration-200",
                            gameStatus === "check" ? "opacity-100" : "opacity-0 invisible",
                        )}
                    >
                        🔥 王手がかかっています
                    </div>
                </div>

                {/* ゲーム終了情報 - 常にスペースを確保 */}
                <div className="mt-2 min-h-[24px] sm:min-h-[32px]">
                    <div
                        className={cn(
                            "space-y-1 transition-all duration-200",
                            isGameOver ? "opacity-100" : "opacity-0 invisible",
                        )}
                    >
                        <div className="text-gray-500 text-xs sm:text-sm bg-gray-50 px-2 sm:px-3 py-1 rounded-full inline-block">
                            🏁 ゲーム終了
                        </div>
                        {(gameStatus === "black_win" || gameStatus === "white_win") && (
                            <div className="text-xs sm:text-sm text-gray-600">
                                第{turn}手までで決着
                            </div>
                        )}
                    </div>
                </div>
            </div>

            {/* ヘルプ・タイマーボタン */}
            <div className="mb-3 flex gap-2 justify-center">
                <KeyboardHelp />
                {!timer.config.mode && gameMode !== "review" && (
                    <button
                        type="button"
                        onClick={() => setIsTimerSettingsOpen(true)}
                        className={cn(
                            "p-2 rounded-lg transition-colors",
                            "touch-manipulation active:scale-95",
                            "bg-gray-100 hover:bg-gray-200 text-gray-700",
                            "focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-gray-500",
                        )}
                        title="タイマー設定"
                    >
                        ⏱️
                    </button>
                )}
                {timer.config.mode && !isGameOver && (
                    <button
                        type="button"
                        onClick={timer.isPaused ? resumeTimer : pauseTimer}
                        className={cn(
                            "p-2 rounded-lg transition-colors",
                            "touch-manipulation active:scale-95",
                            timer.isPaused
                                ? "bg-green-100 hover:bg-green-200 text-green-700"
                                : "bg-yellow-100 hover:bg-yellow-200 text-yellow-700",
                            "focus:outline-none focus:ring-2 focus:ring-offset-2",
                            timer.isPaused ? "focus:ring-green-500" : "focus:ring-yellow-500",
                        )}
                        title={timer.isPaused ? "再開" : "一時停止"}
                    >
                        {timer.isPaused ? "▶️" : "⏸️"}
                    </button>
                )}
            </div>

            {/* モード別操作ボタン */}
            {(gameMode === "review" || gameMode === "analysis") && onStartFromPosition && (
                <div className="mb-3 flex gap-2 justify-center">
                    <button
                        type="button"
                        onClick={onStartFromPosition}
                        className={cn(
                            "px-4 py-2 rounded-lg font-medium transition-colors text-sm",
                            "touch-manipulation active:scale-95",
                            "bg-purple-500 text-white hover:bg-purple-600",
                            "focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-purple-500",
                        )}
                    >
                        🎯 この局面から対局開始
                    </button>
                    {gameMode === "analysis" && hasReviewBase && onReturnToReview && (
                        <button
                            type="button"
                            onClick={onReturnToReview}
                            className={cn(
                                "px-4 py-2 rounded-lg font-medium transition-colors text-sm",
                                "touch-manipulation active:scale-95",
                                "bg-gray-500 text-white hover:bg-gray-600",
                                "focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-gray-500",
                            )}
                        >
                            👀 閲覧モードに戻る
                        </button>
                    )}
                </div>
            )}

            {/* 操作ボタン */}
            <div className="flex gap-2 justify-center">
                {/* 投了ボタン - 対局モードのゲーム中のみ表示（詰将棋モードと閲覧モードでは非表示） */}
                {onResign &&
                    !isGameOver &&
                    moveCount > 0 &&
                    !isTsumeShogi &&
                    gameMode === "playing" && (
                        <AlertDialog open={isResignDialogOpen} onOpenChange={setIsResignDialogOpen}>
                            <AlertDialogTrigger asChild>
                                <button
                                    type="button"
                                    className={cn(
                                        "px-4 sm:px-6 py-2 rounded-lg font-medium transition-colors text-sm sm:text-base",
                                        "touch-manipulation active:scale-95",
                                        "bg-red-500 text-white hover:bg-red-600 focus:ring-red-500",
                                        "focus:outline-none focus:ring-2 focus:ring-offset-2",
                                    )}
                                >
                                    投了
                                </button>
                            </AlertDialogTrigger>
                            <AlertDialogContent>
                                <AlertDialogHeader>
                                    <AlertDialogTitle>投了しますか？</AlertDialogTitle>
                                    <AlertDialogDescription>
                                        投了すると相手の勝ちとなり、対局が終了します。この操作は取り消せません。
                                    </AlertDialogDescription>
                                </AlertDialogHeader>
                                <AlertDialogFooter>
                                    <AlertDialogCancel>キャンセル</AlertDialogCancel>
                                    <AlertDialogAction
                                        onClick={handleConfirmResign}
                                        className="bg-red-500 hover:bg-red-600"
                                    >
                                        投了する
                                    </AlertDialogAction>
                                </AlertDialogFooter>
                            </AlertDialogContent>
                        </AlertDialog>
                    )}

                {/* リセット/新しいゲームボタン */}
                <AlertDialog open={isResetDialogOpen} onOpenChange={setIsResetDialogOpen}>
                    <AlertDialogTrigger asChild>
                        <button
                            type="button"
                            onClick={handleResetClick}
                            className={cn(
                                "px-4 sm:px-6 py-2 rounded-lg font-medium transition-colors text-sm sm:text-base",
                                "touch-manipulation active:scale-95",
                                isGameOver
                                    ? "bg-green-500 text-white hover:bg-green-600 focus:ring-green-500"
                                    : "bg-blue-500 text-white hover:bg-blue-600 focus:ring-blue-500",
                                "focus:outline-none focus:ring-2 focus:ring-offset-2",
                            )}
                        >
                            {isGameOver ? "新しいゲーム" : "リセット"}
                        </button>
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                        <AlertDialogHeader>
                            <AlertDialogTitle>ゲームをリセットしますか？</AlertDialogTitle>
                            <AlertDialogDescription>
                                現在の対局がリセットされ、すべての手順が失われます。この操作は元に戻せません。
                            </AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                            <AlertDialogCancel>キャンセル</AlertDialogCancel>
                            <AlertDialogAction onClick={handleConfirmReset}>
                                リセットする
                            </AlertDialogAction>
                        </AlertDialogFooter>
                    </AlertDialogContent>
                </AlertDialog>
            </div>

            {/* タイマー設定ダイアログ */}
            <TimerSettingsDialog
                open={isTimerSettingsOpen}
                onOpenChange={setIsTimerSettingsOpen}
                isGameInProgress={hasMovesPlayed && !isGameOver}
            />
        </div>
    );
}
