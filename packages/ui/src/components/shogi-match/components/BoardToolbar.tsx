import type { ReactElement } from "react";
import type { DisplaySettings, SquareNotation } from "../types";

interface BoardToolbarProps {
    /** 盤面反転状態 */
    flipBoard: boolean;
    /** 盤面反転変更ハンドラ */
    onFlipBoardChange: (flip: boolean) => void;
    /** 表示設定 */
    displaySettings: DisplaySettings;
    /** 表示設定変更ハンドラ */
    onDisplaySettingsChange: (settings: DisplaySettings) => void;
}

const NOTATION_OPTIONS: { value: SquareNotation; label: string }[] = [
    { value: "none", label: "非表示" },
    { value: "sfen", label: "SFEN" },
    { value: "japanese", label: "日本式" },
];

/**
 * 盤面ツールバーコンポーネント
 * 反転・座標表示・ラベル表示などのクイック設定を提供
 */
export function BoardToolbar({
    flipBoard,
    onFlipBoardChange,
    displaySettings,
    onDisplaySettingsChange,
}: BoardToolbarProps): ReactElement {
    const handleNotationChange = (value: SquareNotation) => {
        onDisplaySettingsChange({
            ...displaySettings,
            squareNotation: value,
        });
    };

    const handleLabelsChange = (checked: boolean) => {
        onDisplaySettingsChange({
            ...displaySettings,
            showBoardLabels: checked,
        });
    };

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

            {/* 座標表示セレクト */}
            <label className="flex items-center gap-1.5">
                <span className="text-muted-foreground">座標:</span>
                <select
                    value={displaySettings.squareNotation}
                    onChange={(e) => handleNotationChange(e.target.value as SquareNotation)}
                    className="px-2 py-1 rounded-md border border-wafuu-border bg-card text-[13px] cursor-pointer"
                >
                    {NOTATION_OPTIONS.map((opt) => (
                        <option key={opt.value} value={opt.value}>
                            {opt.label}
                        </option>
                    ))}
                </select>
            </label>

            {/* 盤外ラベルチェック */}
            <label className="flex items-center gap-1 cursor-pointer text-[13px]">
                <input
                    type="checkbox"
                    checked={displaySettings.showBoardLabels}
                    onChange={(e) => handleLabelsChange(e.target.checked)}
                />
                <span>ラベル</span>
            </label>
        </div>
    );
}
