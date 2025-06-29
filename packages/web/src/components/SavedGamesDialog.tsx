import { useCallback, useEffect, useState } from "react";
import type { Move } from "shogi-core";
import { parseKifMoves } from "shogi-core";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
} from "./ui/alert-dialog";
import { Button } from "./ui/button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "./ui/dialog";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "./ui/table";

interface SavedGame {
    id: string;
    date: string;
    kif: string;
    metadata: {
        blackPlayer: string;
        whitePlayer: string;
        result?: string;
        date: string;
    };
    isOnline: boolean;
    moveCount?: number;
}

interface SavedGamesDialogProps {
    open: boolean;
    onOpenChange: (open: boolean) => void;
    onImport: (moves: Move[], kifContent: string) => void;
    hasActiveGame: boolean;
}

export function SavedGamesDialog({
    open,
    onOpenChange,
    onImport,
    hasActiveGame,
}: SavedGamesDialogProps) {
    const [savedGames, setSavedGames] = useState<SavedGame[]>([]);
    const [isDeleteAllDialogOpen, setIsDeleteAllDialogOpen] = useState(false);
    const [isConfirmLoadDialogOpen, setIsConfirmLoadDialogOpen] = useState(false);
    const [gameToLoad, setGameToLoad] = useState<SavedGame | null>(null);

    // ローカルストレージから保存済みゲームを読み込み
    const loadSavedGames = useCallback(() => {
        try {
            const games = JSON.parse(localStorage.getItem("shogiGames") || "[]");
            setSavedGames(games);
        } catch (error) {
            console.error("Failed to load saved games:", error);
            setSavedGames([]);
        }
    }, []);

    useEffect(() => {
        if (open) {
            loadSavedGames();
        }
    }, [open, loadSavedGames]);

    // ゲームを削除
    const deleteGame = (id: string) => {
        try {
            const filteredGames = savedGames.filter((game) => game.id !== id);
            localStorage.setItem("shogiGames", JSON.stringify(filteredGames));
            setSavedGames(filteredGames);
        } catch (error) {
            console.error("Failed to delete game:", error);
        }
    };

    // すべてのゲームを削除
    const deleteAllGames = () => {
        try {
            localStorage.setItem("shogiGames", "[]");
            setSavedGames([]);
            setIsDeleteAllDialogOpen(false);
        } catch (error) {
            console.error("Failed to delete all games:", error);
        }
    };

    // ゲームを読み込み
    const handleLoadGame = (game: SavedGame) => {
        if (hasActiveGame) {
            setGameToLoad(game);
            setIsConfirmLoadDialogOpen(true);
        } else {
            loadGame(game);
        }
    };

    const loadGame = (game: SavedGame) => {
        try {
            const result = parseKifMoves(game.kif);
            onImport(result.moves, game.kif);
            onOpenChange(false);
            setIsConfirmLoadDialogOpen(false);
            setGameToLoad(null);
        } catch (error) {
            console.error("Failed to load game:", error);
        }
    };

    // 日付をフォーマット
    const formatDate = (dateString: string) => {
        const date = new Date(dateString);
        return date.toLocaleString("ja-JP", {
            year: "numeric",
            month: "2-digit",
            day: "2-digit",
            hour: "2-digit",
            minute: "2-digit",
        });
    };

    return (
        <>
            <Dialog open={open} onOpenChange={onOpenChange}>
                <DialogContent className="max-w-4xl max-h-[80vh] flex flex-col">
                    <DialogHeader>
                        <DialogTitle>保存済み棋譜</DialogTitle>
                        <DialogDescription>
                            過去の対局履歴から棋譜を選択して読み込むことができます
                        </DialogDescription>
                    </DialogHeader>

                    <div className="flex-1 overflow-auto">
                        {savedGames.length === 0 ? (
                            <div className="text-center py-8 text-gray-500">
                                保存された棋譜はありません
                            </div>
                        ) : (
                            <Table>
                                <TableHeader>
                                    <TableRow>
                                        <TableHead className="w-[180px]">日時</TableHead>
                                        <TableHead>先手</TableHead>
                                        <TableHead>後手</TableHead>
                                        <TableHead className="w-[100px] text-center">
                                            手数
                                        </TableHead>
                                        <TableHead className="w-[120px]">結果</TableHead>
                                        <TableHead className="w-[60px] text-center">種別</TableHead>
                                        <TableHead className="w-[140px] text-center">
                                            アクション
                                        </TableHead>
                                    </TableRow>
                                </TableHeader>
                                <TableBody>
                                    {savedGames.map((game) => (
                                        <TableRow key={game.id}>
                                            <TableCell className="font-mono text-sm">
                                                {formatDate(game.date)}
                                            </TableCell>
                                            <TableCell>{game.metadata.blackPlayer}</TableCell>
                                            <TableCell>{game.metadata.whitePlayer}</TableCell>
                                            <TableCell className="text-center">
                                                {game.moveCount || "-"}
                                            </TableCell>
                                            <TableCell>{game.metadata.result || "-"}</TableCell>
                                            <TableCell className="text-center">
                                                {game.isOnline ? (
                                                    <span className="text-blue-600 font-medium">
                                                        通信
                                                    </span>
                                                ) : (
                                                    <span className="text-gray-600">ローカル</span>
                                                )}
                                            </TableCell>
                                            <TableCell>
                                                <div className="flex gap-2 justify-center">
                                                    <Button
                                                        size="sm"
                                                        variant="outline"
                                                        onClick={() => handleLoadGame(game)}
                                                    >
                                                        読込
                                                    </Button>
                                                    <Button
                                                        size="sm"
                                                        variant="outline"
                                                        className="text-red-600 hover:bg-red-50"
                                                        onClick={() => deleteGame(game.id)}
                                                    >
                                                        削除
                                                    </Button>
                                                </div>
                                            </TableCell>
                                        </TableRow>
                                    ))}
                                </TableBody>
                            </Table>
                        )}
                    </div>

                    <DialogFooter>
                        <div className="flex gap-2 justify-between w-full">
                            <Button
                                variant="outline"
                                onClick={() => setIsDeleteAllDialogOpen(true)}
                                disabled={savedGames.length === 0}
                                className="text-red-600 hover:bg-red-50"
                            >
                                すべて削除
                            </Button>
                            <Button onClick={() => onOpenChange(false)}>閉じる</Button>
                        </div>
                    </DialogFooter>
                </DialogContent>
            </Dialog>

            {/* 全削除確認ダイアログ */}
            <AlertDialog open={isDeleteAllDialogOpen} onOpenChange={setIsDeleteAllDialogOpen}>
                <AlertDialogContent>
                    <AlertDialogHeader>
                        <AlertDialogTitle>すべての棋譜を削除しますか？</AlertDialogTitle>
                        <AlertDialogDescription>
                            保存されているすべての棋譜（{savedGames.length}
                            件）が削除されます。この操作は取り消せません。
                        </AlertDialogDescription>
                    </AlertDialogHeader>
                    <AlertDialogFooter>
                        <AlertDialogCancel>キャンセル</AlertDialogCancel>
                        <AlertDialogAction
                            onClick={deleteAllGames}
                            className="bg-red-500 hover:bg-red-600"
                        >
                            すべて削除
                        </AlertDialogAction>
                    </AlertDialogFooter>
                </AlertDialogContent>
            </AlertDialog>

            {/* 対局中の読み込み確認ダイアログ */}
            <AlertDialog open={isConfirmLoadDialogOpen} onOpenChange={setIsConfirmLoadDialogOpen}>
                <AlertDialogContent>
                    <AlertDialogHeader>
                        <AlertDialogTitle>現在の対局を破棄しますか？</AlertDialogTitle>
                        <AlertDialogDescription>
                            現在の対局が破棄され、選択した棋譜が読み込まれます。この操作は取り消せません。
                        </AlertDialogDescription>
                    </AlertDialogHeader>
                    <AlertDialogFooter>
                        <AlertDialogCancel>キャンセル</AlertDialogCancel>
                        <AlertDialogAction onClick={() => gameToLoad && loadGame(gameToLoad)}>
                            棋譜を読み込む
                        </AlertDialogAction>
                    </AlertDialogFooter>
                </AlertDialogContent>
            </AlertDialog>
        </>
    );
}
