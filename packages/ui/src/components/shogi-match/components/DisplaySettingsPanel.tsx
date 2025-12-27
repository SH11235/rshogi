import { detectParallelism } from "@shogi/app-core";
import type { ReactElement } from "react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../../collapsible";
import { RadioGroup, RadioGroupItem } from "../../radio-group";
import type { AnalysisSettings, DisplaySettings, SquareNotation } from "../types";

interface DisplaySettingsPanelProps {
    /** パネル開閉状態 */
    isOpen: boolean;
    /** パネル開閉ハンドラ */
    onOpenChange: (open: boolean) => void;
    /** 表示設定 */
    settings: DisplaySettings;
    /** 表示設定変更ハンドラ */
    onSettingsChange: (settings: DisplaySettings) => void;
    /** 解析設定（オプション） */
    analysisSettings?: AnalysisSettings;
    /** 解析設定変更ハンドラ（オプション） */
    onAnalysisSettingsChange?: (settings: AnalysisSettings) => void;
}

const NOTATION_OPTIONS: { value: SquareNotation; label: string; example: string }[] = [
    { value: "none", label: "非表示", example: "" },
    { value: "sfen", label: "SFEN形式", example: "5e" },
    { value: "japanese", label: "日本式", example: "５五" },
];

const PARALLEL_WORKER_OPTIONS: { value: number; label: string }[] = [
    { value: 0, label: "自動" },
    { value: 1, label: "1" },
    { value: 2, label: "2" },
    { value: 3, label: "3" },
    { value: 4, label: "4" },
];

const ANALYSIS_TIME_OPTIONS: { value: number; label: string }[] = [
    { value: 500, label: "0.5秒" },
    { value: 1000, label: "1秒" },
    { value: 2000, label: "2秒" },
    { value: 3000, label: "3秒" },
];

/**
 * 表示設定パネルコンポーネント
 */
export function DisplaySettingsPanel({
    isOpen,
    onOpenChange,
    settings,
    onSettingsChange,
    analysisSettings,
    onAnalysisSettingsChange,
}: DisplaySettingsPanelProps): ReactElement {
    const parallelismConfig = detectParallelism();
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

    const handleWheelNavigationChange = (checked: boolean) => {
        onSettingsChange({
            ...settings,
            enableWheelNavigation: checked,
        });
    };

    const handleParallelWorkersChange = (value: number) => {
        if (analysisSettings && onAnalysisSettingsChange) {
            onAnalysisSettingsChange({
                ...analysisSettings,
                parallelWorkers: value,
            });
        }
    };

    const handleAnalysisTimeChange = (value: number) => {
        if (analysisSettings && onAnalysisSettingsChange) {
            onAnalysisSettingsChange({
                ...analysisSettings,
                batchAnalysisTimeMs: value,
            });
        }
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

                        {/* 操作設定 */}
                        <div className="flex flex-col gap-2">
                            <div className="text-[13px] font-semibold text-wafuu-sumi mb-1">
                                操作設定
                            </div>
                            <label className="flex items-center gap-2 text-[13px] cursor-pointer py-1">
                                <input
                                    type="checkbox"
                                    checked={settings.enableWheelNavigation}
                                    onChange={(e) => handleWheelNavigationChange(e.target.checked)}
                                />
                                <span>
                                    マウスホイールで棋譜をナビゲート
                                    <span className="text-xs text-muted-foreground ml-1">
                                        (将棋盤エリア上で有効)
                                    </span>
                                </span>
                            </label>
                        </div>

                        {/* 解析設定（オプション） */}
                        {analysisSettings && onAnalysisSettingsChange && (
                            <>
                                <div className="border-t border-wafuu-border my-2" />
                                <div className="flex flex-col gap-2">
                                    <div className="text-[13px] font-semibold text-wafuu-sumi mb-1">
                                        一括解析設定
                                    </div>

                                    {/* 並列ワーカー数 */}
                                    <div className="flex items-center gap-2 text-[13px]">
                                        <span className="text-wafuu-sumi min-w-[80px]">
                                            並列数:
                                        </span>
                                        <div className="flex gap-1">
                                            {PARALLEL_WORKER_OPTIONS.map((opt) => (
                                                <button
                                                    key={opt.value}
                                                    type="button"
                                                    onClick={() =>
                                                        handleParallelWorkersChange(opt.value)
                                                    }
                                                    className={`px-2 py-1 rounded text-xs transition-colors ${
                                                        analysisSettings.parallelWorkers ===
                                                        opt.value
                                                            ? "bg-wafuu-kincha text-white"
                                                            : "bg-wafuu-washi text-wafuu-sumi hover:bg-wafuu-border"
                                                    }`}
                                                >
                                                    {opt.value === 0
                                                        ? `自動(${parallelismConfig.recommendedWorkers})`
                                                        : opt.label}
                                                </button>
                                            ))}
                                        </div>
                                    </div>

                                    {/* 解析時間 */}
                                    <div className="flex items-center gap-2 text-[13px]">
                                        <span className="text-wafuu-sumi min-w-[80px]">
                                            解析時間:
                                        </span>
                                        <div className="flex gap-1">
                                            {ANALYSIS_TIME_OPTIONS.map((opt) => (
                                                <button
                                                    key={opt.value}
                                                    type="button"
                                                    onClick={() =>
                                                        handleAnalysisTimeChange(opt.value)
                                                    }
                                                    className={`px-2 py-1 rounded text-xs transition-colors ${
                                                        analysisSettings.batchAnalysisTimeMs ===
                                                        opt.value
                                                            ? "bg-wafuu-kincha text-white"
                                                            : "bg-wafuu-washi text-wafuu-sumi hover:bg-wafuu-border"
                                                    }`}
                                                >
                                                    {opt.label}
                                                </button>
                                            ))}
                                        </div>
                                    </div>

                                    <div className="text-xs text-muted-foreground mt-1">
                                        検出コア数: {parallelismConfig.detectedConcurrency}
                                    </div>
                                </div>
                            </>
                        )}
                    </div>
                </CollapsibleContent>
            </div>
        </Collapsible>
    );
}
