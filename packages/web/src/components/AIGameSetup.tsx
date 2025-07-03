import { useGameStore } from "@/stores/gameStore";
import type { AIDifficulty } from "@/types/ai";
import { useState } from "react";
import type { Player } from "shogi-core";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";

export function AIGameSetup() {
    const { startAIGame, gameType } = useGameStore();
    const [difficulty, setDifficulty] = useState<AIDifficulty>("intermediate");
    const [playerColor, setPlayerColor] = useState<Player>("black");
    const [isStarting, setIsStarting] = useState(false);

    const handleStartGame = async () => {
        setIsStarting(true);
        try {
            await startAIGame(difficulty, playerColor);
        } catch (error) {
            console.error("Failed to start AI game:", error);
        } finally {
            setIsStarting(false);
        }
    };

    if (gameType === "ai") {
        return null; // AI game already in progress
    }

    return (
        <Card>
            <CardHeader>
                <CardTitle>AI対戦</CardTitle>
                <CardDescription>コンピューター対戦の設定を選択してください</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
                <div className="space-y-2">
                    <label htmlFor="difficulty" className="text-sm font-medium">
                        難易度
                    </label>
                    <Select
                        value={difficulty}
                        onValueChange={(value) => setDifficulty(value as AIDifficulty)}
                    >
                        <SelectTrigger id="difficulty">
                            <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                            <SelectItem value="beginner">初心者</SelectItem>
                            <SelectItem value="intermediate">中級者</SelectItem>
                            <SelectItem value="advanced">上級者</SelectItem>
                            <SelectItem value="expert">エキスパート</SelectItem>
                        </SelectContent>
                    </Select>
                </div>

                <div className="space-y-2">
                    <label htmlFor="color" className="text-sm font-medium">
                        手番
                    </label>
                    <Select
                        value={playerColor}
                        onValueChange={(value) => setPlayerColor(value as Player)}
                    >
                        <SelectTrigger id="color">
                            <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                            <SelectItem value="black">先手 (☗)</SelectItem>
                            <SelectItem value="white">後手 (☖)</SelectItem>
                        </SelectContent>
                    </Select>
                </div>

                <Button onClick={handleStartGame} disabled={isStarting} className="w-full">
                    {isStarting ? "準備中..." : "対局開始"}
                </Button>
            </CardContent>
        </Card>
    );
}
