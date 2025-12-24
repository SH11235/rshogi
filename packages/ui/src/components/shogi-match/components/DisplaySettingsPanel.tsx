import type { ReactElement } from "react";
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
            <div className="bg-wafuu-washi-warm border-2 border-wafuu-border rounded-xl overflow-hidden shadow-lg w-[var(--panel-width)]">
                <CollapsibleTrigger asChild>
                    <button
                        type="button"
                        aria-label="表示設定パネルを開閉"
                        className={`w-full px-4 py-3.5 bg-gradient-to-br from-wafuu-washi to-wafuu-washi-warm border-none flex items-center justify-between gap-3 cursor-pointer transition-all duration-200 ${
                            isOpen ? "border-b border-wafuu-border" : ""
                        }`}
                    >
                        <span className="flex items-center gap-3">
                            <span className="text-lg font-bold text-wafuu-sumi tracking-wide">
                                表示設定
                            </span>
                            <span className="text-sm font-semibold text-wafuu-kincha">
                                {summary}
                            </span>
                        </span>
                        <span
                            aria-hidden="true"
                            className={`text-xl text-wafuu-kincha shrink-0 transition-transform duration-200 ${
                                isOpen ? "rotate-180" : "rotate-0"
                            }`}
                        >
                            ▼
                        </span>
                    </button>
                </CollapsibleTrigger>
                <CollapsibleContent>
                    <div className="p-4 flex flex-col gap-4">
                        {/* 座標表示 */}
                        <div className="flex flex-col gap-2">
                            <div className="text-[13px] font-semibold text-wafuu-sumi mb-1">
                                座標表示
                            </div>
                            <RadioGroup
                                value={settings.squareNotation}
                                onValueChange={(v) => handleNotationChange(v as SquareNotation)}
                            >
                                {NOTATION_OPTIONS.map((opt) => (
                                    <div
                                        key={opt.value}
                                        className="flex items-center gap-2 text-[13px] cursor-pointer py-1"
                                    >
                                        <RadioGroupItem
                                            value={opt.value}
                                            id={`notation-${opt.value}`}
                                        />
                                        <label htmlFor={`notation-${opt.value}`}>{opt.label}</label>
                                        {opt.example && (
                                            <span className="text-xs text-muted-foreground ml-1">
                                                ({opt.example})
                                            </span>
                                        )}
                                    </div>
                                ))}
                            </RadioGroup>
                        </div>

                        {/* 盤外ラベル */}
                        <div className="flex flex-col gap-2">
                            <div className="text-[13px] font-semibold text-wafuu-sumi mb-1">
                                盤外ラベル
                            </div>
                            <label className="flex items-center gap-2 text-[13px] cursor-pointer py-1">
                                <input
                                    type="checkbox"
                                    checked={settings.showBoardLabels}
                                    onChange={(e) => handleLabelsChange(e.target.checked)}
                                />
                                <span>筋(1~9)・段(一~九)を表示</span>
                            </label>
                        </div>

                        {/* ハイライト */}
                        <div className="flex flex-col gap-2">
                            <div className="text-[13px] font-semibold text-wafuu-sumi mb-1">
                                ハイライト
                            </div>
                            <label className="flex items-center gap-2 text-[13px] cursor-pointer py-1">
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
