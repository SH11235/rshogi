import { detectParallelism } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useState } from "react";
import { Popover, PopoverContent, PopoverTrigger } from "../../popover";
import { RadioGroup, RadioGroupItem } from "../../radio-group";
import type { AnalysisSettings, DisplaySettings, SquareNotation } from "../types";

interface AppMenuProps {
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
 * SquareNotation型ガード関数
 */
const isSquareNotation = (v: string): v is SquareNotation => {
    return v === "none" || v === "sfen" || v === "japanese";
};

/**
 * アプリメニューコンポーネント（左上ハンバーガーメニュー）
 * 表示設定と解析設定を含む
 */
export function AppMenu({
    settings,
    onSettingsChange,
    analysisSettings,
    onAnalysisSettingsChange,
}: AppMenuProps): ReactElement {
    const [isOpen, setIsOpen] = useState(false);
    const parallelismConfig = detectParallelism();

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
        <Popover open={isOpen} onOpenChange={setIsOpen}>
            <PopoverTrigger asChild>
                <button
                    type="button"
                    className="
                        w-10 h-10 flex items-center justify-center
                        text-xl text-wafuu-sumi
                        bg-wafuu-washi hover:bg-wafuu-washi-warm
                        border border-wafuu-border rounded-lg
                        cursor-pointer transition-colors duration-150
                        shadow-sm
                    "
                    aria-label="メニューを開く"
                >
                    ☰
                </button>
            </PopoverTrigger>
            <PopoverContent
                side="bottom"
                align="start"
                className="w-72 p-0 bg-wafuu-washi-warm border-2 border-wafuu-border rounded-xl shadow-lg"
            >
                <div className="p-3 border-b border-wafuu-border bg-gradient-to-br from-wafuu-washi to-wafuu-washi-warm">
                    <span className="text-lg font-bold text-wafuu-sumi tracking-wide">設定</span>
                </div>
                <div className="p-4 flex flex-col gap-4 max-h-[70vh] overflow-y-auto">
                    {/* 座標表示 */}
                    <div className="flex flex-col gap-2">
                        <div className="text-[13px] font-semibold text-wafuu-sumi mb-1">
                            座標表示
                        </div>
                        <RadioGroup
                            value={settings.squareNotation}
                            onValueChange={(v) => {
                                if (isSquareNotation(v)) {
                                    handleNotationChange(v);
                                }
                            }}
                        >
                            {NOTATION_OPTIONS.map((opt) => (
                                <div
                                    key={opt.value}
                                    className="flex items-center gap-2 text-[13px] cursor-pointer py-1"
                                >
                                    <RadioGroupItem
                                        value={opt.value}
                                        id={`menu-notation-${opt.value}`}
                                    />
                                    <label htmlFor={`menu-notation-${opt.value}`}>
                                        {opt.label}
                                    </label>
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
                        <label
                            htmlFor="board-labels-toggle"
                            className="flex items-center gap-2 text-[13px] cursor-pointer py-1"
                        >
                            <input
                                id="board-labels-toggle"
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
                        <label
                            htmlFor="highlight-last-move-toggle"
                            className="flex items-center gap-2 text-[13px] cursor-pointer py-1"
                        >
                            <input
                                id="highlight-last-move-toggle"
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
                        <label
                            htmlFor="wheel-navigation-toggle"
                            className="flex items-center gap-2 text-[13px] cursor-pointer py-1"
                        >
                            <input
                                id="wheel-navigation-toggle"
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
                                    <span className="text-wafuu-sumi min-w-[80px]">並列数:</span>
                                    <div className="flex gap-1 flex-wrap">
                                        {PARALLEL_WORKER_OPTIONS.map((opt) => (
                                            <button
                                                key={opt.value}
                                                type="button"
                                                onClick={() =>
                                                    handleParallelWorkersChange(opt.value)
                                                }
                                                className={`px-2 py-1 rounded text-xs transition-colors ${
                                                    analysisSettings.parallelWorkers === opt.value
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
                                    <span className="text-wafuu-sumi min-w-[80px]">解析時間:</span>
                                    <div className="flex gap-1 flex-wrap">
                                        {ANALYSIS_TIME_OPTIONS.map((opt) => (
                                            <button
                                                key={opt.value}
                                                type="button"
                                                onClick={() => handleAnalysisTimeChange(opt.value)}
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
            </PopoverContent>
        </Popover>
    );
}
