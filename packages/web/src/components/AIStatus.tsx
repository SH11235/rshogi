import { useGameStore } from "@/stores/gameStore";
import { Loader2 } from "lucide-react";
import { Card, CardContent } from "./ui/card";

export function AIStatus() {
    const { gameType, aiPlayerInfo, isAIThinking, currentPlayer, localPlayerColor } =
        useGameStore();

    // Only show for AI games
    if (gameType !== "ai" || !aiPlayerInfo) {
        return null;
    }

    // Check if it's AI's turn
    const isAITurn = currentPlayer !== localPlayerColor;

    return (
        <Card className="mb-4">
            <CardContent className="pt-6">
                <div className="flex items-center justify-between">
                    <div>
                        <p className="text-sm text-muted-foreground">AI対戦中</p>
                        <p className="font-medium">{aiPlayerInfo.name}</p>
                    </div>
                    {isAIThinking && isAITurn && (
                        <div className="flex items-center space-x-2">
                            <Loader2 className="h-4 w-4 animate-spin" />
                            <span className="text-sm">思考中...</span>
                        </div>
                    )}
                </div>
                {aiPlayerInfo.lastEvaluation && (
                    <div className="mt-4 space-y-1 text-sm text-muted-foreground">
                        <p>評価値: {aiPlayerInfo.lastEvaluation.score}</p>
                        <p>探索深度: {aiPlayerInfo.lastEvaluation.depth}</p>
                        <p>探索ノード数: {aiPlayerInfo.lastEvaluation.nodes.toLocaleString()}</p>
                        <p>思考時間: {aiPlayerInfo.lastEvaluation.time}ms</p>
                    </div>
                )}
            </CardContent>
        </Card>
    );
}
