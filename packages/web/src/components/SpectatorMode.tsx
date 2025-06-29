import { useState } from "react";
import { Button } from "./ui/button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";

interface SpectatorModeProps {
    onJoinAsSpectator: (gameId: string) => void;
    onHostSpectatorGame: () => string;
    spectatorCount?: number;
    isSpectator?: boolean;
}

export function SpectatorMode({
    onJoinAsSpectator,
    onHostSpectatorGame,
    spectatorCount = 0,
    isSpectator = false,
}: SpectatorModeProps) {
    const [isJoinDialogOpen, setIsJoinDialogOpen] = useState(false);
    const [isHostDialogOpen, setIsHostDialogOpen] = useState(false);
    const [gameId, setGameId] = useState("");
    const [hostGameId, setHostGameId] = useState("");

    const handleHostGame = () => {
        const newGameId = onHostSpectatorGame();
        setHostGameId(newGameId);
        setIsHostDialogOpen(true);
    };

    const handleJoinGame = () => {
        if (gameId.trim()) {
            onJoinAsSpectator(gameId.trim());
            setIsJoinDialogOpen(false);
            setGameId("");
        }
    };

    return (
        <>
            {/* 観戦者数表示 */}
            {spectatorCount > 0 && (
                <div className="mb-2 text-center">
                    <span className="text-sm text-gray-600 bg-gray-100 px-3 py-1 rounded-full">
                        👥 観戦者: {spectatorCount}人
                    </span>
                </div>
            )}

            {/* 観戦モード表示 */}
            {isSpectator && (
                <div className="mb-4 text-center">
                    <div className="text-sm text-purple-600 font-bold bg-purple-50 px-3 py-1 rounded-full inline-block">
                        👁️ 観戦中
                    </div>
                </div>
            )}

            {/* 観戦機能ボタン */}
            {!isSpectator && (
                <div className="flex gap-2 justify-center mb-4">
                    <Button
                        onClick={() => setIsJoinDialogOpen(true)}
                        variant="outline"
                        size="sm"
                        className="text-purple-600 border-purple-300 hover:bg-purple-50"
                    >
                        👁️ 観戦する
                    </Button>
                    <Button
                        onClick={handleHostGame}
                        variant="outline"
                        size="sm"
                        className="text-green-600 border-green-300 hover:bg-green-50"
                    >
                        📺 観戦可能にする
                    </Button>
                </div>
            )}

            {/* 観戦参加ダイアログ */}
            <Dialog open={isJoinDialogOpen} onOpenChange={setIsJoinDialogOpen}>
                <DialogContent>
                    <DialogHeader>
                        <DialogTitle>対局を観戦</DialogTitle>
                        <DialogDescription>観戦したい対局のIDを入力してください</DialogDescription>
                    </DialogHeader>
                    <div className="grid gap-4 py-4">
                        <div className="grid gap-2">
                            <label htmlFor="gameId" className="text-sm font-medium">
                                対局ID
                            </label>
                            <Input
                                id="gameId"
                                value={gameId}
                                onChange={(e) => setGameId(e.target.value)}
                                placeholder="対局IDを入力"
                                onKeyDown={(e) => {
                                    if (e.key === "Enter") {
                                        handleJoinGame();
                                    }
                                }}
                            />
                        </div>
                    </div>
                    <DialogFooter>
                        <Button variant="outline" onClick={() => setIsJoinDialogOpen(false)}>
                            キャンセル
                        </Button>
                        <Button onClick={handleJoinGame} disabled={!gameId.trim()}>
                            観戦開始
                        </Button>
                    </DialogFooter>
                </DialogContent>
            </Dialog>

            {/* 観戦可能対局IDダイアログ */}
            <Dialog open={isHostDialogOpen} onOpenChange={setIsHostDialogOpen}>
                <DialogContent>
                    <DialogHeader>
                        <DialogTitle>観戦可能な対局</DialogTitle>
                        <DialogDescription>以下のIDを観戦者に共有してください</DialogDescription>
                    </DialogHeader>
                    <div className="py-4">
                        <div className="bg-gray-100 p-4 rounded-lg text-center">
                            <p className="text-xs text-gray-600 mb-2">対局ID</p>
                            <p className="text-2xl font-mono font-bold text-gray-800 select-all">
                                {hostGameId}
                            </p>
                        </div>
                        <p className="text-sm text-gray-600 mt-4 text-center">
                            この対局は観戦可能な状態になりました
                        </p>
                    </div>
                    <DialogFooter>
                        <Button onClick={() => setIsHostDialogOpen(false)}>閉じる</Button>
                    </DialogFooter>
                </DialogContent>
            </Dialog>
        </>
    );
}
