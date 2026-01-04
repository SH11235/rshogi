import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";
import { useEffect, useRef } from "react";

export interface KifuMove {
    /** 手数（1から始まる） */
    ply: number;
    /** 日本語表記の指し手 */
    displayText: string;
}

export interface MobileKifuBarProps {
    /** 棋譜データ */
    moves: KifuMove[];
    /** 現在の手数 */
    currentPly: number;
    /** 手数選択時のコールバック */
    onPlySelect?: (ply: number) => void;
}

/**
 * スマホ対局モード用の1行棋譜表示
 * 横スクロールで全体を表示し、現在手を中央に配置
 */
export function MobileKifuBar({
    moves,
    currentPly,
    onPlySelect,
}: MobileKifuBarProps): ReactElement {
    const containerRef = useRef<HTMLDivElement>(null);
    const currentRef = useRef<HTMLButtonElement>(null);

    // 現在の手を中央にスクロール
    useEffect(() => {
        if (currentRef.current && containerRef.current) {
            const container = containerRef.current;
            const current = currentRef.current;
            const containerWidth = container.clientWidth;
            const currentLeft = current.offsetLeft;
            const currentWidth = current.clientWidth;
            // 現在の手を中央に配置
            const scrollLeft = currentLeft - containerWidth / 2 + currentWidth / 2;
            container.scrollTo({ left: scrollLeft, behavior: "smooth" });
        }
    }, [currentPly]);

    if (moves.length === 0) {
        return (
            <div className="h-9 flex items-center justify-center text-sm text-muted-foreground bg-muted/30 rounded">
                開始局面
            </div>
        );
    }

    return (
        <div
            ref={containerRef}
            className="h-9 flex items-center gap-1 overflow-x-auto scrollbar-hide bg-muted/30 rounded px-2"
            style={{ scrollbarWidth: "none", msOverflowStyle: "none" }}
        >
            {moves.map((move) => {
                const isCurrent = move.ply === currentPly;
                return (
                    <button
                        key={move.ply}
                        ref={isCurrent ? currentRef : undefined}
                        type="button"
                        onClick={() => onPlySelect?.(move.ply)}
                        className={cn(
                            "shrink-0 px-2 py-1 rounded text-sm whitespace-nowrap transition-colors",
                            isCurrent
                                ? "bg-primary text-primary-foreground font-semibold"
                                : "text-foreground hover:bg-muted",
                        )}
                    >
                        {move.displayText}
                    </button>
                );
            })}
        </div>
    );
}
