import { cn } from "@/lib/utils";
import { useGameStore } from "@/stores/gameStore";
import { useRef, useState } from "react";
import { exportToKif, parseKifMoves, validateKifFormat } from "shogi-core";
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
    onReset: () => void;
    onResign?: () => void;
    onImportGame?: (moves: Move[]) => void;
}

export function GameInfo({
    currentPlayer,
    gameStatus,
    moveHistory,
    historyCursor,
    resignedPlayer,
    onReset,
    onResign,
    onImportGame,
}: GameInfoProps) {
    const moveCount = moveHistory.length;
    // Calculate turn based on current position in history
    // When historyCursor is -1 (latest position), show next move number
    // When historyCursor >= 0, show the position after that move
    const turn =
        historyCursor === -1 || historyCursor === undefined ? moveCount + 1 : historyCursor + 2;
    const [isResetDialogOpen, setIsResetDialogOpen] = useState(false);
    const [isResignDialogOpen, setIsResignDialogOpen] = useState(false);
    const fileInputRef = useRef<HTMLInputElement>(null);
    const [isTimerSettingsOpen, setIsTimerSettingsOpen] = useState(false);
    const timer = useGameStore((state) => state.timer);
    const pauseTimer = useGameStore((state) => state.pauseTimer);
    const resumeTimer = useGameStore((state) => state.resumeTimer);

    const getStatusMessage = () => {
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

    // 棋譜エクスポート
    const handleExportKif = () => {
        if (moveHistory.length === 0) {
            alert("エクスポートする棋譜がありません");
            return;
        }

        const kifContent = exportToKif(moveHistory, {
            開始日時: new Date().toLocaleString("ja-JP"),
            先手: "先手",
            後手: "後手",
            棋戦: "自由対局",
            手合割: "平手",
        });

        const blob = new Blob([kifContent], { type: "text/plain;charset=utf-8" });
        const url = window.URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = `shogi_game_${new Date().toISOString().split("T")[0]}.kif`;
        document.body.appendChild(link);
        link.click();
        document.body.removeChild(link);
        window.URL.revokeObjectURL(url);
    };

    // 棋譜インポート
    const handleImportKif = () => {
        fileInputRef.current?.click();
    };

    const handleFileSelect = (event: React.ChangeEvent<HTMLInputElement>) => {
        const file = event.target.files?.[0];
        if (!file) return;

        const reader = new FileReader();
        reader.onload = (e) => {
            const content = e.target?.result as string;

            // KIF形式のバリデーション
            const validation = validateKifFormat(content);
            if (!validation.valid) {
                alert(`KIFファイルの形式が無効です: ${validation.error}`);
                return;
            }

            try {
                const moves = parseKifMoves(content);
                if (moves.length === 0) {
                    alert("棋譜から手順を読み取れませんでした");
                    return;
                }

                if (onImportGame) {
                    onImportGame(moves);
                }
            } catch (error) {
                console.error("KIF parsing error:", error);
                alert("棋譜の解析中にエラーが発生しました");
            }
        };

        reader.readAsText(file, "utf-8");
        // ファイル選択をリセット
        event.target.value = "";
    };

    return (
        <div className="p-3 sm:p-6 bg-white rounded-lg shadow-md max-w-xs sm:max-w-md mx-auto">
            {/* タイマー表示 */}
            {timer.config.mode && (
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

            {/* ゲーム状態表示 */}
            <div className="mb-3 sm:mb-4 text-center">
                <h2
                    className={cn(
                        "text-lg sm:text-xl lg:text-2xl font-bold mb-2",
                        getStatusColor(),
                    )}
                >
                    {getStatusMessage()}
                </h2>

                {/* 詳細メッセージ */}
                {getDetailedMessage() && (
                    <p className="text-xs sm:text-sm text-gray-600 mb-2">{getDetailedMessage()}</p>
                )}

                {/* 手数表示 */}
                <div className="flex justify-center gap-2 sm:gap-4 text-xs sm:text-sm text-gray-600 mb-2 sm:mb-3">
                    <span>第{turn}手</span>
                    <span>•</span>
                    <span>総手数: {moveCount}</span>
                </div>

                {/* 追加の状態情報 */}
                {gameStatus === "check" && (
                    <div className="text-red-500 text-xs sm:text-sm font-medium bg-red-50 px-2 sm:px-3 py-1 rounded-full">
                        🔥 王手がかかっています
                    </div>
                )}

                {isGameOver && (
                    <div className="mt-2 space-y-1">
                        <div className="text-gray-500 text-xs sm:text-sm bg-gray-50 px-2 sm:px-3 py-1 rounded-full">
                            🏁 ゲーム終了
                        </div>
                        {(gameStatus === "black_win" || gameStatus === "white_win") && (
                            <div className="text-xs sm:text-sm text-gray-600">
                                第{turn}手までで決着
                            </div>
                        )}
                    </div>
                )}
            </div>

            {/* 棋譜エクスポート/インポートボタン */}
            {moveHistory.length > 0 && (
                <div className="mb-3 flex gap-2 justify-center">
                    <button
                        type="button"
                        onClick={handleExportKif}
                        className={cn(
                            "px-3 sm:px-4 py-1.5 rounded-md font-medium transition-colors text-xs sm:text-sm",
                            "touch-manipulation active:scale-95",
                            "bg-gray-100 text-gray-700 hover:bg-gray-200",
                            "focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-gray-500",
                        )}
                    >
                        📥 棋譜保存
                    </button>
                    {onImportGame && (
                        <>
                            <button
                                type="button"
                                onClick={handleImportKif}
                                className={cn(
                                    "px-3 sm:px-4 py-1.5 rounded-md font-medium transition-colors text-xs sm:text-sm",
                                    "touch-manipulation active:scale-95",
                                    "bg-gray-100 text-gray-700 hover:bg-gray-200",
                                    "focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-gray-500",
                                )}
                            >
                                📤 棋譜読込
                            </button>
                            <input
                                type="file"
                                ref={fileInputRef}
                                onChange={handleFileSelect}
                                accept=".kif"
                                style={{ display: "none" }}
                            />
                        </>
                    )}
                </div>
            )}

            {/* ヘルプ・タイマーボタン */}
            <div className="mb-3 flex gap-2 justify-center">
                <KeyboardHelp />
                {!timer.config.mode && (
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

            {/* 操作ボタン */}
            <div className="flex gap-2 justify-center">
                {/* 投了ボタン - ゲーム中のみ表示 */}
                {onResign && !isGameOver && moveCount > 0 && (
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
