import { openingBook } from "@/services/openingBook";
import type { BookLevel, BookMove, LoadProgress } from "@/types/openingBook";
import { useCallback, useEffect, useState } from "react";

export function useOpeningBook(sfen: string) {
    const [moves, setMoves] = useState<BookMove[]>([]);
    const [loading, setLoading] = useState(false);
    const [progress, setProgress] = useState<LoadProgress | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [level, setLevel] = useState<BookLevel>("early");

    // 初期化
    useEffect(() => {
        openingBook
            .initialize()
            .then(() => openingBook.loadBook("early"))
            .catch((err) => {
                console.error("Failed to initialize opening book:", err);
                setError(err.message || "Failed to initialize");
            });
    }, []);

    // 局面変更時の検索
    useEffect(() => {
        if (!sfen) return;

        openingBook
            .findMoves(sfen)
            .then(setMoves)
            .catch((err) => {
                console.error("Failed to find moves:", err);
                setMoves([]);
            });
    }, [sfen]);

    const loadMoreData = useCallback(
        async (newLevel: BookLevel) => {
            if (loading || level === newLevel) return;

            setLoading(true);
            setError(null);

            try {
                await openingBook.loadBook(newLevel, setProgress);
                setLevel(newLevel);
            } catch (err) {
                console.error("Failed to load opening book data:", err);
                setError(err instanceof Error ? err.message : "Failed to load data");
            } finally {
                setLoading(false);
                setProgress(null);
            }
        },
        [loading, level],
    );

    return {
        moves,
        loading,
        progress,
        error,
        level,
        loadMoreData,
    };
}
