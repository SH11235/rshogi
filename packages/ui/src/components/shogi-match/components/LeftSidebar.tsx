import type { NnueMeta } from "@shogi/app-core";
import { detectParallelism } from "@shogi/app-core";
import type { SkillLevelSettings } from "@shogi/engine-client";
import type { ReactElement } from "react";
import { Input } from "../../input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../../select";
import type { ClockSettings } from "../hooks/useClockManager";
import type { AnalysisSettings, PassRightsSettings, SideSetting } from "../types";
import { PlayerIcon } from "./PlayerIcon";
import { SkillLevelSelector } from "./SkillLevelSelector";

type SideKey = "sente" | "gote";

interface LeftSidebarProps {
    // 対局設定
    sides: { sente: SideSetting; gote: SideSetting };
    onSidesChange: (sides: { sente: SideSetting; gote: SideSetting }) => void;
    timeSettings: ClockSettings;
    onTimeSettingsChange: (settings: ClockSettings) => void;
    passRightsSettings?: PassRightsSettings;
    onPassRightsSettingsChange?: (settings: PassRightsSettings) => void;
    settingsLocked: boolean;

    // NNUE 一覧
    nnueList: NnueMeta[];

    // 対局用 NNUE（先手・後手で共通）
    matchNnueId: string | null;
    onMatchNnueIdChange: (id: string | null) => void;

    // 分析設定
    analysisSettings: AnalysisSettings;
    onAnalysisSettingsChange: (settings: AnalysisSettings) => void;
    analysisNnueId: string | null;
    onAnalysisNnueIdChange: (id: string | null) => void;

    // NNUE 管理
    onOpenNnueManager: () => void;

    // 表示設定
    onOpenDisplaySettings: () => void;

    // パス権設定
    onOpenPassRightsSettings: () => void;
}

const PARALLEL_WORKER_OPTIONS = [
    { value: 0, label: "自動" },
    { value: 1, label: "1" },
    { value: 2, label: "2" },
    { value: 3, label: "3" },
    { value: 4, label: "4" },
];

const ANALYSIS_TIME_OPTIONS = [
    { value: 500, label: "0.5秒" },
    { value: 1000, label: "1秒" },
    { value: 2000, label: "2秒" },
    { value: 3000, label: "3秒" },
];

const sectionClassName = "flex flex-col gap-3";
const sectionTitleClassName = "text-sm font-semibold text-wafuu-sumi";
const labelClassName = "flex flex-col gap-1 text-xs text-muted-foreground";
const inputClassName = "border border-wafuu-border bg-wafuu-washi text-sm text-xs";

/**
 * 左サイドバーコンポーネント
 * 対局設定、分析設定、NNUE管理、表示設定を含む
 */
export function LeftSidebar({
    sides,
    onSidesChange,
    timeSettings,
    onTimeSettingsChange,
    passRightsSettings,
    onPassRightsSettingsChange,
    settingsLocked,
    nnueList,
    matchNnueId,
    onMatchNnueIdChange,
    analysisSettings,
    onAnalysisSettingsChange,
    analysisNnueId,
    onAnalysisNnueIdChange,
    onOpenNnueManager,
    onOpenDisplaySettings,
    onOpenPassRightsSettings,
}: LeftSidebarProps): ReactElement {
    const parallelismConfig = detectParallelism();

    // プレイヤー選択の値を生成: "human", "material", "nnue:{nnueId}"
    // matchNnueId を使用（先手・後手で共通の NNUE を使用）
    const getSelectorValue = (setting: SideSetting): string => {
        if (setting.role === "human") return "human";
        if (matchNnueId === null) return "material";
        return `nnue:${matchNnueId}`;
    };

    const handlePlayerChange = (side: SideKey, value: string) => {
        const currentSetting = sides[side];
        if (value === "human") {
            onSidesChange({
                ...sides,
                [side]: {
                    role: "human",
                    engineId: undefined,
                    skillLevel: undefined,
                },
            });
        } else if (value === "material") {
            onMatchNnueIdChange(null);
            onSidesChange({
                ...sides,
                [side]: {
                    role: "engine",
                    engineId: "internal",
                    skillLevel: currentSetting.skillLevel,
                },
            });
        } else if (value.startsWith("nnue:")) {
            const nnueId = value.slice("nnue:".length);
            onMatchNnueIdChange(nnueId);
            onSidesChange({
                ...sides,
                [side]: {
                    role: "engine",
                    engineId: "internal",
                    skillLevel: currentSetting.skillLevel,
                },
            });
        }
    };

    const handleSkillLevelChange = (side: SideKey, skillLevel: SkillLevelSettings | undefined) => {
        onSidesChange({
            ...sides,
            [side]: { ...sides[side], skillLevel },
        });
    };

    const handleTimeChange = (side: SideKey, field: "mainMs" | "byoyomiMs", inputValue: string) => {
        const parsed = Number(inputValue);
        if (Number.isNaN(parsed) || parsed < 0) return;
        const MAX_SECONDS = 86400;
        const clampedSeconds = Math.min(Math.floor(parsed), MAX_SECONDS);
        onTimeSettingsChange({
            ...timeSettings,
            [side]: {
                ...timeSettings[side],
                [field]: clampedSeconds * 1000,
            },
        });
    };

    const sideColumn = (side: SideKey, label: string, colorClass: string, hasBorder: boolean) => {
        const setting = sides[side];
        const selectorValue = getSelectorValue(setting);

        return (
            <div
                className={`flex flex-col gap-2 ${hasBorder ? "border-r-2 border-wafuu-sumi/20 pr-3" : "pl-3"}`}
            >
                <div className={`text-xs font-semibold ${colorClass}`}>{label}</div>
                <div className={labelClassName}>
                    <span>プレイヤー</span>
                    <Select
                        value={selectorValue}
                        onValueChange={(value) => handlePlayerChange(side, value)}
                        disabled={settingsLocked}
                    >
                        <SelectTrigger>
                            <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                            <SelectItem value="human">人間</SelectItem>
                            <SelectItem value="material">
                                <span className="flex items-center gap-1.5">
                                    <PlayerIcon side="sente" isAI showBorder={false} size="xs" />
                                    Material
                                </span>
                            </SelectItem>
                            {nnueList.map((nnue) => (
                                <SelectItem key={nnue.id} value={`nnue:${nnue.id}`}>
                                    <span className="flex items-center gap-1.5">
                                        <PlayerIcon
                                            side="sente"
                                            isAI
                                            showBorder={false}
                                            size="xs"
                                        />
                                        {nnue.displayName}
                                    </span>
                                </SelectItem>
                            ))}
                        </SelectContent>
                    </Select>
                </div>
                {/* レイアウトシフト防止のため固定高さを確保 */}
                <div className="min-h-[4rem]">
                    {setting.role === "engine" && (
                        <SkillLevelSelector
                            value={setting.skillLevel}
                            onChange={(skillLevel) => handleSkillLevelChange(side, skillLevel)}
                            disabled={settingsLocked}
                        />
                    )}
                </div>
                <div className={labelClassName}>
                    <span>持ち時間(秒)</span>
                    <Input
                        type="number"
                        min={0}
                        max={86400}
                        value={Math.floor(timeSettings[side].mainMs / 1000)}
                        disabled={settingsLocked}
                        className={inputClassName}
                        onChange={(e) => handleTimeChange(side, "mainMs", e.target.value)}
                    />
                </div>
                <div className={labelClassName}>
                    <span>秒読み(秒)</span>
                    <Input
                        type="number"
                        min={0}
                        max={86400}
                        value={Math.floor(timeSettings[side].byoyomiMs / 1000)}
                        disabled={settingsLocked}
                        className={inputClassName}
                        onChange={(e) => handleTimeChange(side, "byoyomiMs", e.target.value)}
                    />
                </div>
            </div>
        );
    };

    return (
        <div className="w-96 self-center overflow-y-auto bg-wafuu-washi-warm border border-wafuu-border rounded-xl p-4 flex flex-col gap-6">
            {/* 対局設定 */}
            <div className={sectionClassName}>
                <div className={sectionTitleClassName}>対局設定</div>
                {settingsLocked && (
                    <div className="flex items-center gap-2 rounded-lg bg-wafuu-sumi/10 px-3 py-1.5 text-xs text-muted-foreground">
                        <span>対局中は変更不可</span>
                    </div>
                )}
                {/* 先手/後手設定 */}
                <div className="grid grid-cols-2">
                    {sideColumn("sente", "☗先手", "text-wafuu-shu", true)}
                    {sideColumn("gote", "☖後手", "text-wafuu-ai", false)}
                </div>

                {/* 変則ルール */}
                {passRightsSettings && onPassRightsSettingsChange && (
                    <button
                        type="button"
                        onClick={onOpenPassRightsSettings}
                        disabled={settingsLocked}
                        className="w-full text-left px-3 py-2 rounded-lg text-sm text-wafuu-sumi bg-wafuu-washi border-2 border-wafuu-border shadow-sm hover:shadow-md hover:-translate-y-0.5 hover:border-wafuu-kincha transition-all disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:shadow-sm disabled:hover:translate-y-0 disabled:hover:border-wafuu-border flex items-center gap-2"
                    >
                        <span>🎲</span>
                        <span>
                            変則ルール...
                            {passRightsSettings.enabled && (
                                <span className="ml-2 text-xs text-muted-foreground">
                                    (パス権: {passRightsSettings.initialCount}回)
                                </span>
                            )}
                        </span>
                    </button>
                )}
            </div>

            {/* NNUE 管理 */}
            <button
                type="button"
                onClick={onOpenNnueManager}
                className="w-full text-left px-3 py-2 rounded-lg text-sm text-wafuu-sumi bg-wafuu-washi border-2 border-wafuu-border shadow-sm hover:shadow-md hover:-translate-y-0.5 hover:border-wafuu-kincha transition-all flex items-center gap-2"
            >
                <span>📁</span>
                <span>NNUE 管理...</span>
            </button>

            {/* 分析設定 */}
            <div className={sectionClassName}>
                <div className={sectionTitleClassName}>分析設定</div>

                {/* 分析用 NNUE 選択 */}
                <div className={labelClassName}>
                    <span>
                        将棋エンジンの評価関数（
                        <button
                            type="button"
                            onClick={onOpenNnueManager}
                            className="text-wafuu-ai hover:underline"
                        >
                            NNUE管理
                        </button>
                        から追加）
                    </span>
                    <Select
                        value={analysisNnueId ?? "material"}
                        onValueChange={(value) =>
                            onAnalysisNnueIdChange(value === "material" ? null : value)
                        }
                    >
                        <SelectTrigger>
                            <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                            <SelectItem value="material">Material</SelectItem>
                            {nnueList.map((nnue) => (
                                <SelectItem key={nnue.id} value={nnue.id}>
                                    {nnue.displayName}
                                </SelectItem>
                            ))}
                        </SelectContent>
                    </Select>
                </div>

                {/* 並列数 */}
                <div className="flex flex-col gap-1">
                    <span className="text-xs text-muted-foreground">並列数</span>
                    <div className="flex gap-1 flex-wrap">
                        {PARALLEL_WORKER_OPTIONS.map((opt) => (
                            <button
                                key={opt.value}
                                type="button"
                                onClick={() =>
                                    onAnalysisSettingsChange({
                                        ...analysisSettings,
                                        parallelWorkers: opt.value,
                                    })
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
                <div className="flex flex-col gap-1">
                    <span className="text-xs text-muted-foreground">解析時間</span>
                    <div className="flex gap-1 flex-wrap">
                        {ANALYSIS_TIME_OPTIONS.map((opt) => (
                            <button
                                key={opt.value}
                                type="button"
                                onClick={() =>
                                    onAnalysisSettingsChange({
                                        ...analysisSettings,
                                        batchAnalysisTimeMs: opt.value,
                                    })
                                }
                                className={`px-2 py-1 rounded text-xs transition-colors ${
                                    analysisSettings.batchAnalysisTimeMs === opt.value
                                        ? "bg-wafuu-kincha text-white"
                                        : "bg-wafuu-washi text-wafuu-sumi hover:bg-wafuu-border"
                                }`}
                            >
                                {opt.label}
                            </button>
                        ))}
                    </div>
                </div>
            </div>

            {/* 表示設定 */}
            <button
                type="button"
                onClick={onOpenDisplaySettings}
                className="w-full text-left px-3 py-2 rounded-lg text-sm text-wafuu-sumi bg-wafuu-washi border-2 border-wafuu-border shadow-sm hover:shadow-md hover:-translate-y-0.5 hover:border-wafuu-kincha transition-all flex items-center gap-2"
            >
                <span>👁️</span>
                <span>表示設定...</span>
            </button>
        </div>
    );
}
