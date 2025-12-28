import type { ReactElement } from "react";

interface BoardToolbarProps {
    /** 盤面反転状態 */
    flipBoard: boolean;
    /** 盤面反転変更ハンドラ */
    onFlipBoardChange: (flip: boolean) => void;
}

/**
 * 盤面ツールバーコンポーネント
 * 反転ボタンを提供
 */
export function BoardToolbar({ flipBoard, onFlipBoardChange }: BoardToolbarProps): ReactElement {
    return (
        <div className="flex items-center gap-3 px-3 py-2 bg-wafuu-washi-warm border border-wafuu-border rounded-lg text-[13px]">
            {/* 反転ボタン */}
            <button
                type="button"
                onClick={() => onFlipBoardChange(!flipBoard)}
                className={`flex items-center gap-1 px-2 py-1 rounded-md border border-wafuu-border cursor-pointer text-[13px] transition-all duration-150 ${
                    flipBoard ? "bg-wafuu-kin/20" : "bg-card"
                }`}
                aria-pressed={flipBoard}
                title="盤面を反転"
            >
                <span aria-hidden="true" className="text-sm">
                    🔄
                </span>
                <span>反転</span>
            </button>
        </div>
    );
}
