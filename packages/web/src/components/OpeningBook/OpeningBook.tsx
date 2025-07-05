import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Progress } from "@/components/ui/progress";
import { useOpeningBook } from "@/hooks/useOpeningBook";
import { useGameStore } from "@/stores/gameStore";

export function OpeningBook() {
    const sfen = useGameStore((state) => state.getCurrentSfen?.() || "");
    const makeMove = useGameStore((state) => state.makeMove);
    const { moves, loading, error, progress, level, loadMoreData } = useOpeningBook(sfen);

    const handleMoveClick = (notation: string) => {
        makeMove?.(notation);
    };

    const formatEval = (evaluation: number): string => {
        const sign = evaluation > 0 ? "+" : "";
        return `${sign}${evaluation}`;
    };

    const levels = [
        { value: "early", label: "序盤" },
        { value: "standard", label: "標準" },
        { value: "full", label: "完全版" },
    ] as const;

    return (
        <Card>
            <CardHeader>
                <CardTitle>定跡</CardTitle>
            </CardHeader>
            <CardContent>
                <div className="flex gap-2 mb-4">
                    {levels.map((levelOption) => (
                        <Button
                            key={levelOption.value}
                            variant={level === levelOption.value ? "default" : "outline"}
                            size="sm"
                            onClick={() => loadMoreData(levelOption.value)}
                            disabled={loading}
                        >
                            {levelOption.label}
                        </Button>
                    ))}
                </div>

                {loading && progress && (
                    <div className="mb-4">
                        <Progress
                            value={
                                progress.phase === "downloading"
                                    ? (progress.loaded / progress.total) * 100
                                    : 50
                            }
                        />
                        <p className="text-sm text-muted-foreground mt-1">
                            {progress.phase === "downloading" ? "読み込み中" : "解凍中"}...
                        </p>
                    </div>
                )}

                {error && <div className="mb-4 text-destructive">エラー: {error}</div>}

                <div className="space-y-2">
                    {moves.map((move) => (
                        <Button
                            key={`${move.notation}-${move.evaluation}-${move.depth}`}
                            variant="outline"
                            className="w-full justify-between"
                            onClick={() => handleMoveClick(move.notation)}
                        >
                            <span className="font-mono">{move.notation}</span>
                            <span className="text-sm text-muted-foreground">
                                評価: {formatEval(move.evaluation)} 深さ: {move.depth}
                            </span>
                        </Button>
                    ))}
                </div>
            </CardContent>
        </Card>
    );
}
