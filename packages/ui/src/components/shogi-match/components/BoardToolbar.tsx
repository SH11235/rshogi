import type { CSSProperties, ReactElement } from "react";
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

const toolbarStyle: CSSProperties = {
    display: "flex",
    alignItems: "center",
    gap: "12px",
    padding: "8px 12px",
    background: "hsl(var(--wafuu-washi-warm))",
    border: "1px solid hsl(var(--wafuu-border))",
    borderRadius: "8px",
    fontSize: "13px",
};

const buttonStyle: CSSProperties = {
    display: "flex",
    alignItems: "center",
    gap: "4px",
    padding: "4px 8px",
    borderRadius: "6px",
    border: "1px solid hsl(var(--wafuu-border))",
    background: "hsl(var(--card, 0 0% 100%))",
    cursor: "pointer",
    fontSize: "13px",
    transition: "all 0.15s ease",
};

const selectStyle: CSSProperties = {
    padding: "4px 8px",
    borderRadius: "6px",
    border: "1px solid hsl(var(--wafuu-border))",
    background: "hsl(var(--card, 0 0% 100%))",
    fontSize: "13px",
    cursor: "pointer",
};

const checkboxLabelStyle: CSSProperties = {
    display: "flex",
    alignItems: "center",
    gap: "4px",
    cursor: "pointer",
    fontSize: "13px",
};

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
        <div style={toolbarStyle}>
            {/* 反転ボタン */}
            <button
                type="button"
                onClick={() => onFlipBoardChange(!flipBoard)}
                style={{
                    ...buttonStyle,
                    background: flipBoard
                        ? "hsl(var(--wafuu-kin) / 0.2)"
                        : "hsl(var(--card, 0 0% 100%))",
                }}
                aria-pressed={flipBoard}
                title="盤面を反転"
            >
                <span aria-hidden="true" style={{ fontSize: "14px" }}>
                    🔄
                </span>
                <span>反転</span>
            </button>

            {/* 座標表示セレクト */}
            <label style={{ display: "flex", alignItems: "center", gap: "6px" }}>
                <span style={{ color: "hsl(var(--muted-foreground))" }}>座標:</span>
                <select
                    value={displaySettings.squareNotation}
                    onChange={(e) => handleNotationChange(e.target.value as SquareNotation)}
                    style={selectStyle}
                >
                    {NOTATION_OPTIONS.map((opt) => (
                        <option key={opt.value} value={opt.value}>
                            {opt.label}
                        </option>
                    ))}
                </select>
            </label>

            {/* 盤外ラベルチェック */}
            <label style={checkboxLabelStyle}>
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
