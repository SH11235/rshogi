import { useGameStore } from "@/stores/gameStore";
import { useCallback, useEffect, useState } from "react";
import type { GameStatus, Move } from "shogi-core";
import { numberToKanji, pieceTypeToKanji } from "shogi-core";
import { Button } from "./ui/button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "./ui/dialog";
import { Textarea } from "./ui/textarea";

interface GameRecordManagerProps {
    autoSave?: boolean;
    onGameEnd?: (kifContent: string) => void;
}

// Simple KIF format generation
interface KifMetadata {
    date: string;
    blackPlayer: string;
    whitePlayer: string;
    result?: string;
    timeControl?: {
        mode: string;
        basicTime: number;
        byoyomiTime: number;
    };
}

function generateKifFormat(moves: Move[], metadata: KifMetadata): string {
    let kif = "";

    // Header
    kif += "# KIF形式\n";
    kif += `開始日時：${metadata.date}\n`;
    kif += `先手：${metadata.blackPlayer}\n`;
    kif += `後手：${metadata.whitePlayer}\n`;
    if (metadata.result) {
        kif += `結果：${metadata.result}\n`;
    }
    if (metadata.timeControl) {
        kif += `持ち時間：${metadata.timeControl.basicTime}分+${metadata.timeControl.byoyomiTime}秒\n`;
    }
    kif += "\n";

    // Moves
    moves.forEach((move, index) => {
        const turn = index % 2 === 0 ? "▲" : "△";
        if (move.type === "drop") {
            const pieceName = pieceTypeToKanji(move.piece.type);
            const toCol = fullWidthNumber(move.to.column);
            const toRow = numberToKanji(move.to.row);
            kif += `${index + 1} ${turn}${toCol}${toRow}${pieceName}打\n`;
        } else if (move.type === "move") {
            const pieceName = pieceTypeToKanji(move.piece.type);
            const toCol = fullWidthNumber(move.to.column);
            const toRow = numberToKanji(move.to.row);
            const fromStr = `(${move.from.column}${move.from.row})`;
            const promotion = move.promote ? "成" : "";
            kif += `${index + 1} ${turn}${toCol}${toRow}${pieceName}${promotion}${fromStr}\n`;
        }
    });

    return kif;
}

function fullWidthNumber(num: number): string {
    const fullWidthNumbers = ["０", "１", "２", "３", "４", "５", "６", "７", "８", "９"];
    return fullWidthNumbers[num];
}

export function GameRecordManager({ autoSave = true, onGameEnd }: GameRecordManagerProps) {
    const [isShareDialogOpen, setIsShareDialogOpen] = useState(false);
    const [kifContent, setKifContent] = useState("");
    const [shareUrl, setShareUrl] = useState("");
    const [message, setMessage] = useState("");

    const moveHistory = useGameStore((state) => state.moveHistory);
    const gameStatus = useGameStore((state) => state.gameStatus);
    const timer = useGameStore((state) => state.timer);
    const localPlayer = useGameStore((state) => state.localPlayer);
    const isOnlineGame = useGameStore((state) => state.isOnlineGame);
    const gameMode = useGameStore((state) => state.gameMode);

    // ゲーム結果のテキストを取得
    const getGameResultText = useCallback((status: GameStatus): string => {
        switch (status) {
            case "black_win":
                return "先手勝ち";
            case "white_win":
                return "後手勝ち";
            case "draw":
            case "sennichite":
                return "引き分け";
            case "perpetual_check":
                return "連続王手の千日手";
            case "timeout":
                return "時間切れ";
            case "resigned":
                return "投了";
            default:
                return "";
        }
    }, []);

    // ゲーム終了時の自動保存
    useEffect(() => {
        if (autoSave && gameMode === "playing" && !["playing", "check"].includes(gameStatus)) {
            const metadata = {
                blackPlayer: localPlayer === "black" ? "あなた" : "相手",
                whitePlayer: localPlayer === "white" ? "あなた" : "相手",
                date: new Date().toISOString().split("T")[0],
                result: getGameResultText(gameStatus as GameStatus),
                timeControl: timer.config.mode
                    ? {
                          mode: timer.config.mode,
                          basicTime: timer.config.basicTime,
                          byoyomiTime: timer.config.byoyomiTime,
                      }
                    : undefined,
            };

            const kif = generateKifFormat(moveHistory as Move[], metadata);
            setKifContent(kif);

            // ローカルストレージに保存
            saveToLocalStorage(kif, metadata);

            // コールバックを呼ぶ
            if (onGameEnd) {
                onGameEnd(kif);
            }

            setMessage("棋譜が自動的に保存されました");
            setTimeout(() => setMessage(""), 3000);
        }
    }, [
        gameStatus,
        gameMode,
        autoSave,
        moveHistory,
        localPlayer,
        timer.config,
        onGameEnd,
        getGameResultText,
    ]);

    // ローカルストレージに保存
    function saveToLocalStorage(kif: string, metadata: KifMetadata) {
        try {
            const savedGames = JSON.parse(localStorage.getItem("shogiGames") || "[]");
            const newGame = {
                id: Date.now().toString(),
                date: new Date().toISOString(),
                kif,
                metadata,
                isOnline: isOnlineGame,
            };

            savedGames.unshift(newGame);

            // 最大100件まで保存
            if (savedGames.length > 100) {
                savedGames.length = 100;
            }

            localStorage.setItem("shogiGames", JSON.stringify(savedGames));
        } catch (error) {
            console.error("Failed to save game to localStorage:", error);
        }
    }

    // 現在の棋譜を生成
    const generateCurrentKif = useCallback(() => {
        const metadata = {
            blackPlayer: localPlayer === "black" ? "あなた" : isOnlineGame ? "相手" : "先手",
            whitePlayer: localPlayer === "white" ? "あなた" : isOnlineGame ? "相手" : "後手",
            date: new Date().toISOString().split("T")[0],
            result: getGameResultText(gameStatus as GameStatus),
        };

        return generateKifFormat(moveHistory as Move[], metadata);
    }, [moveHistory, gameStatus, localPlayer, isOnlineGame, getGameResultText]);

    // 棋譜をクリップボードにコピー
    const copyToClipboard = async () => {
        try {
            await navigator.clipboard.writeText(kifContent);
            setMessage("棋譜をクリップボードにコピーしました");
            setTimeout(() => setMessage(""), 3000);
        } catch (error) {
            setMessage("クリップボードへのアクセスが拒否されました");
            setTimeout(() => setMessage(""), 3000);
        }
    };

    // 棋譜をダウンロード
    const downloadKif = () => {
        const blob = new Blob([kifContent], { type: "text/plain;charset=utf-8" });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = `shogi_${new Date().toISOString().split("T")[0]}.kif`;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        URL.revokeObjectURL(url);

        setMessage("棋譜のダウンロードを開始しました");
        setTimeout(() => setMessage(""), 3000);
    };

    // 棋譜を共有URL生成（実際の実装では外部サービスを使用）
    const generateShareUrl = async () => {
        try {
            // 実際の実装では棋譜共有サービスのAPIを使用
            // ここでは仮のURLを生成
            const mockUrl = `https://shogi-share.example.com/game/${Date.now()}`;
            setShareUrl(mockUrl);

            setMessage("棋譜の共有URLを生成しました");
            setTimeout(() => setMessage(""), 3000);
        } catch (error) {
            setMessage("共有URLの生成に失敗しました");
            setTimeout(() => setMessage(""), 3000);
        }
    };

    // 共有ダイアログを開く
    const openShareDialog = () => {
        const kif = generateCurrentKif();
        setKifContent(kif);
        setIsShareDialogOpen(true);
    };

    return (
        <>
            {/* メッセージ表示 */}
            {message && (
                <div className="fixed top-4 right-4 bg-gray-900 text-white px-4 py-2 rounded shadow-lg z-50">
                    {message}
                </div>
            )}

            {/* 共有ボタン */}
            <Button
                onClick={openShareDialog}
                variant="outline"
                size="sm"
                className="text-blue-600 border-blue-300 hover:bg-blue-50"
            >
                📤 棋譜を共有
            </Button>

            {/* 共有ダイアログ */}
            <Dialog open={isShareDialogOpen} onOpenChange={setIsShareDialogOpen}>
                <DialogContent className="max-w-2xl">
                    <DialogHeader>
                        <DialogTitle>棋譜の共有</DialogTitle>
                        <DialogDescription>
                            棋譜をコピー、ダウンロード、または共有URLを生成できます
                        </DialogDescription>
                    </DialogHeader>

                    <div className="space-y-4">
                        {/* 棋譜表示 */}
                        <div>
                            <label
                                htmlFor="kif-textarea"
                                className="text-sm font-medium mb-2 block"
                            >
                                KIF形式の棋譜
                            </label>
                            <Textarea
                                id="kif-textarea"
                                value={kifContent}
                                readOnly
                                className="font-mono text-sm h-64"
                                onClick={(e) => e.currentTarget.select()}
                            />
                        </div>

                        {/* 共有URL */}
                        {shareUrl && (
                            <div>
                                <label
                                    htmlFor="share-url-input"
                                    className="text-sm font-medium mb-2 block"
                                >
                                    共有URL
                                </label>
                                <div className="flex gap-2">
                                    <input
                                        id="share-url-input"
                                        type="text"
                                        value={shareUrl}
                                        readOnly
                                        className="flex-1 px-3 py-2 border border-gray-300 rounded-md text-sm"
                                        onClick={(e) => e.currentTarget.select()}
                                    />
                                    <Button
                                        onClick={() => navigator.clipboard.writeText(shareUrl)}
                                        size="sm"
                                    >
                                        コピー
                                    </Button>
                                </div>
                            </div>
                        )}

                        {/* アクションボタン */}
                        <div className="flex gap-2 justify-center">
                            <Button onClick={copyToClipboard} variant="outline">
                                📋 クリップボードにコピー
                            </Button>
                            <Button onClick={downloadKif} variant="outline">
                                💾 ダウンロード
                            </Button>
                            {!shareUrl && (
                                <Button onClick={generateShareUrl} variant="outline">
                                    🔗 共有URL生成
                                </Button>
                            )}
                        </div>
                    </div>

                    <DialogFooter>
                        <Button onClick={() => setIsShareDialogOpen(false)}>閉じる</Button>
                    </DialogFooter>
                </DialogContent>
            </Dialog>
        </>
    );
}
