import { AlertCircle, CheckCircle, Loader2, Search, XCircle } from "lucide-react";
import React from "react";
import { useGameStore } from "../stores/gameStore";
import type { MateSearchStatus } from "../types/mateSearch";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";

const statusIcons: Record<MateSearchStatus, React.ReactNode> = {
    idle: <Search className="h-4 w-4" />,
    searching: <Loader2 className="h-4 w-4 animate-spin" />,
    found: <CheckCircle className="h-4 w-4 text-green-600" />,
    not_found: <XCircle className="h-4 w-4 text-yellow-600" />,
    error: <AlertCircle className="h-4 w-4 text-red-600" />,
};

const statusMessages: Record<MateSearchStatus, string> = {
    idle: "詰み探索を開始",
    searching: "探索中...",
    found: "詰みあり",
    not_found: "詰みなし",
    error: "エラー",
};

export function MateSearchPanel() {
    const { mateSearch, startMateSearch, cancelMateSearch, gameStatus } = useGameStore();
    const [maxDepth, setMaxDepth] = React.useState(7);

    const canSearch = gameStatus === "playing" || gameStatus === "check";
    const isSearching = mateSearch.status === "searching";

    const handleSearch = async () => {
        if (isSearching) {
            cancelMateSearch();
        } else {
            await startMateSearch({ maxDepth });
        }
    };

    return (
        <Card className="w-full">
            <CardHeader className="pb-3">
                <CardTitle className="text-lg">詰み探索</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
                <div className="flex items-center gap-2">
                    <label htmlFor="depth" className="text-sm font-medium">
                        探索深さ:
                    </label>
                    <select
                        id="depth"
                        value={maxDepth}
                        onChange={(e) => setMaxDepth(Number(e.target.value))}
                        disabled={isSearching}
                        className="rounded border px-2 py-1 text-sm"
                    >
                        <option value={1}>1手詰め</option>
                        <option value={3}>3手詰め</option>
                        <option value={5}>5手詰め</option>
                        <option value={7}>7手詰め</option>
                        <option value={9}>9手詰め</option>
                    </select>
                </div>

                <Button
                    onClick={handleSearch}
                    disabled={!canSearch && !isSearching}
                    variant={isSearching ? "outline" : "default"}
                    className="w-full"
                >
                    {statusIcons[mateSearch.status]}
                    <span className="ml-2">
                        {isSearching ? "キャンセル" : statusMessages[mateSearch.status]}
                    </span>
                </Button>

                {isSearching && (
                    <div className="text-sm text-muted-foreground text-center">
                        {mateSearch.depth}手詰めまで探索中...
                    </div>
                )}

                {mateSearch.result && (
                    <div className="space-y-2 rounded-lg border p-3">
                        <div className="text-sm font-medium">
                            {mateSearch.result.isMate ? (
                                <span className="text-green-600">
                                    {mateSearch.result.depth}手詰め発見
                                </span>
                            ) : (
                                <span className="text-yellow-600">
                                    {mateSearch.maxDepth}手以内に詰みなし
                                </span>
                            )}
                        </div>

                        {mateSearch.result.isMate && mateSearch.result.moves.length > 0 && (
                            <div className="space-y-1">
                                <div className="text-sm font-medium">詰み手順:</div>
                                <div className="text-xs space-y-0.5">
                                    {mateSearch.result.moves.map((move, index) => (
                                        <div key={`move-${index}-${move}`} className="font-mono">
                                            {index + 1}. {move}
                                        </div>
                                    ))}
                                </div>
                            </div>
                        )}

                        <div className="text-xs text-muted-foreground">
                            探索ノード数: {mateSearch.result.nodeCount.toLocaleString()}
                            <br />
                            実行時間: {mateSearch.result.elapsedMs}ms
                        </div>
                    </div>
                )}

                {mateSearch.error && (
                    <div className="text-sm text-red-600">エラー: {mateSearch.error}</div>
                )}
            </CardContent>
        </Card>
    );
}
