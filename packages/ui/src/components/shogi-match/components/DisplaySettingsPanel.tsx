import type { CSSProperties, ReactElement } from "react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../../collapsible";
import { RadioGroup, RadioGroupItem } from "../../radio-group";
import type { DisplaySettings, SquareNotation } from "../types";

interface DisplaySettingsPanelProps {
    /** パネル開閉状態 */
    isOpen: boolean;
    /** パネル開閉ハンドラ */
    onOpenChange: (open: boolean) => void;
    /** 表示設定 */
    settings: DisplaySettings;
    /** 表示設定変更ハンドラ */
    onSettingsChange: (settings: DisplaySettings) => void;
}

const NOTATION_OPTIONS: { value: SquareNotation; label: string; example: string }[] = [
    { value: "none", label: "非表示", example: "" },
    { value: "sfen", label: "SFEN形式", example: "5e" },
    { value: "japanese", label: "日本式", example: "５五" },
];

const sectionStyle: CSSProperties = {
    display: "flex",
    flexDirection: "column",
    gap: "8px",
};

const sectionTitleStyle: CSSProperties = {
    fontSize: "13px",
    fontWeight: 600,
    color: "hsl(var(--wafuu-sumi))",
    marginBottom: "4px",
};

const radioLabelStyle: CSSProperties = {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    fontSize: "13px",
    cursor: "pointer",
    padding: "4px 0",
};

const checkboxLabelStyle: CSSProperties = {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    fontSize: "13px",
    cursor: "pointer",
    padding: "4px 0",
};

const exampleStyle: CSSProperties = {
    fontSize: "12px",
    color: "hsl(var(--muted-foreground))",
    marginLeft: "4px",
};

/**
 * 表示設定パネルコンポーネント
 */
export function DisplaySettingsPanel({
    isOpen,
    onOpenChange,
    settings,
    onSettingsChange,
}: DisplaySettingsPanelProps): ReactElement {
    // サマリー表示
    const notationLabel =
        NOTATION_OPTIONS.find((opt) => opt.value === settings.squareNotation)?.label ?? "非表示";
    const labelsLabel = settings.showBoardLabels ? "ON" : "OFF";
    const summary = `座標:${notationLabel} | ラベル:${labelsLabel}`;

    const handleNotationChange = (value: SquareNotation) => {
        onSettingsChange({
            ...settings,
            squareNotation: value,
        });
    };

    const handleLabelsChange = (checked: boolean) => {
        onSettingsChange({
            ...settings,
            showBoardLabels: checked,
        });
    };

    const handleHighlightChange = (checked: boolean) => {
        onSettingsChange({
            ...settings,
            highlightLastMove: checked,
        });
    };

    return (
        <Collapsible open={isOpen} onOpenChange={onOpenChange}>
            <div
                style={{
                    background: "hsl(var(--wafuu-washi-warm))",
                    border: "2px solid hsl(var(--wafuu-border))",
                    borderRadius: "12px",
                    overflow: "hidden",
                    boxShadow: "0 8px 20px rgba(0,0,0,0.08)",
                    width: "var(--panel-width)",
                }}
            >
                <CollapsibleTrigger asChild>
                    <button
                        type="button"
                        aria-label="表示設定パネルを開閉"
                        style={{
                            width: "100%",
                            padding: "14px 16px",
                            background:
                                "linear-gradient(135deg, hsl(var(--wafuu-washi)) 0%, hsl(var(--wafuu-washi-warm)) 100%)",
                            border: "none",
                            borderBottom: isOpen ? "1px solid hsl(var(--wafuu-border))" : "none",
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "space-between",
                            gap: "12px",
                            cursor: "pointer",
                            transition: "all 0.2s ease",
                        }}
                    >
                        <span style={{ display: "flex", alignItems: "center", gap: "12px" }}>
                            <span
                                style={{
                                    fontSize: "18px",
                                    fontWeight: 700,
                                    color: "hsl(var(--wafuu-sumi))",
                                    letterSpacing: "0.05em",
                                }}
                            >
                                表示設定
                            </span>
                            <span
                                style={{
                                    fontSize: "14px",
                                    fontWeight: 600,
                                    color: "hsl(var(--wafuu-kincha))",
                                }}
                            >
                                {summary}
                            </span>
                        </span>
                        <span
                            aria-hidden="true"
                            style={{
                                fontSize: "20px",
                                color: "hsl(var(--wafuu-kincha))",
                                transform: isOpen ? "rotate(180deg)" : "rotate(0deg)",
                                transition: "transform 0.2s ease",
                                flexShrink: 0,
                            }}
                        >
                            ▼
                        </span>
                    </button>
                </CollapsibleTrigger>
                <CollapsibleContent>
                    <div
                        style={{
                            padding: "16px",
                            display: "flex",
                            flexDirection: "column",
                            gap: "16px",
                        }}
                    >
                        {/* 座標表示 */}
                        <div style={sectionStyle}>
                            <div style={sectionTitleStyle}>座標表示</div>
                            <RadioGroup
                                value={settings.squareNotation}
                                onValueChange={(v) => handleNotationChange(v as SquareNotation)}
                            >
                                {NOTATION_OPTIONS.map((opt) => (
                                    <div key={opt.value} style={radioLabelStyle}>
                                        <RadioGroupItem
                                            value={opt.value}
                                            id={`notation-${opt.value}`}
                                        />
                                        <label htmlFor={`notation-${opt.value}`}>{opt.label}</label>
                                        {opt.example && (
                                            <span style={exampleStyle}>({opt.example})</span>
                                        )}
                                    </div>
                                ))}
                            </RadioGroup>
                        </div>

                        {/* 盤外ラベル */}
                        <div style={sectionStyle}>
                            <div style={sectionTitleStyle}>盤外ラベル</div>
                            <label style={checkboxLabelStyle}>
                                <input
                                    type="checkbox"
                                    checked={settings.showBoardLabels}
                                    onChange={(e) => handleLabelsChange(e.target.checked)}
                                />
                                <span>筋(1~9)・段(一~九)を表示</span>
                            </label>
                        </div>

                        {/* ハイライト */}
                        <div style={sectionStyle}>
                            <div style={sectionTitleStyle}>ハイライト</div>
                            <label style={checkboxLabelStyle}>
                                <input
                                    type="checkbox"
                                    checked={settings.highlightLastMove}
                                    onChange={(e) => handleHighlightChange(e.target.checked)}
                                />
                                <span>最終手を強調表示</span>
                            </label>
                        </div>
                    </div>
                </CollapsibleContent>
            </div>
        </Collapsible>
    );
}
