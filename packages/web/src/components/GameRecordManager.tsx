import { useGameStore } from "@/stores/gameStore";
import { useCallback, useEffect, useState } from "react";
import type { GameStatus, Move } from "shogi-core";
import { numberToKanji, pieceTypeToKanji } from "shogi-core";

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

    return (
        <>
            {/* メッセージ表示 */}
            {message && (
                <div className="fixed top-4 right-4 bg-gray-900 text-white px-4 py-2 rounded shadow-lg z-50">
                    {message}
                </div>
            )}
        </>
    );
}
