import type { Player } from "@shogi/app-core";
import type { EngineClient, SkillLevelSettings } from "@shogi/engine-client";
import type { ReactElement } from "react";
import { Input } from "../../input";
import type { ClockSettings } from "../hooks/useClockManager";
import { SkillLevelSelector } from "./SkillLevelSelector";

type SideRole = "human" | "engine";

export type SideSetting = {
    role: SideRole;
    engineId?: string;
    /** エンジンの強さ設定（role="engine"時のみ有効） */
    skillLevel?: SkillLevelSettings;
};

export type EngineOption = {
    id: string;
    label: string;
    createClient: () => EngineClient;
    kind?: "internal" | "external";
};

interface MatchSettingsPanelProps {
    // 設定値
    sides: { sente: SideSetting; gote: SideSetting };
    onSidesChange: (sides: { sente: SideSetting; gote: SideSetting }) => void;
    timeSettings: ClockSettings;
    onTimeSettingsChange: (settings: ClockSettings) => void;
    currentTurn: Player;
    onTurnChange: (turn: Player) => void;

    // エンジン情報
    uiEngineOptions: EngineOption[];

    // 制約
    settingsLocked: boolean;
}

const selectClassName =
    "p-2 rounded-lg border border-[hsl(var(--border,0_0%_86%))] bg-[hsl(var(--card,0_0%_100%))] text-sm";
const inputClassName =
    "border border-[hsl(var(--border,0_0%_86%))] bg-[hsl(var(--card,0_0%_100%))] text-sm";
const labelClassName = "flex flex-col gap-1 text-xs text-muted-foreground";

export function MatchSettingsPanel({
    sides,
    onSidesChange,
    timeSettings,
    onTimeSettingsChange,
    currentTurn,
    onTurnChange,
    uiEngineOptions,
    settingsLocked,
}: MatchSettingsPanelProps): ReactElement {
    // 選択肢の値を生成: "human" または "engine:{engineId}"
    const getSelectorValue = (setting: SideSetting): string => {
        if (setting.role === "human") return "human";
        return `engine:${setting.engineId ?? uiEngineOptions[0]?.id ?? ""}`;
    };

    const handleSelectorChange = (side: Player, value: string) => {
        const currentSetting = sides[side];
        if (value === "human") {
            onSidesChange({
                ...sides,
                [side]: { role: "human", engineId: undefined, skillLevel: undefined },
            });
        } else if (value.startsWith("engine:")) {
            const engineId = value.slice("engine:".length);
            onSidesChange({
                ...sides,
                [side]: {
                    role: "engine",
                    engineId,
                    skillLevel: currentSetting.skillLevel,
                },
            });
        }
    };

    const handleSkillLevelChange = (side: Player, skillLevel: SkillLevelSettings | undefined) => {
        onSidesChange({
            ...sides,
            [side]: { ...sides[side], skillLevel },
        });
    };

    const sideSelector = (side: Player) => {
        const setting = sides[side];
        const selectorValue = getSelectorValue(setting);

        return (
            <div className="flex flex-col gap-1.5">
                <label className={labelClassName}>
                    プレイヤー
                    <select
                        value={selectorValue}
                        onChange={(e) => handleSelectorChange(side, e.target.value)}
                        disabled={settingsLocked}
                        className={selectClassName}
                    >
                        <option value="human">人間</option>
                        {uiEngineOptions.map((opt) => (
                            <option key={opt.id} value={`engine:${opt.id}`}>
                                {opt.label}
                            </option>
                        ))}
                    </select>
                </label>
                {setting.role === "engine" && (
                    <SkillLevelSelector
                        value={setting.skillLevel}
                        onChange={(skillLevel) => handleSkillLevelChange(side, skillLevel)}
                        disabled={settingsLocked}
                    />
                )}
            </div>
        );
    };

    const timeSelector = (side: Player) => {
        const settings = timeSettings[side];
        return (
            <div className="flex flex-col gap-1.5">
                <label className={labelClassName}>
                    持ち時間(秒)
                    <Input
                        type="number"
                        min={0}
                        value={Math.floor(settings.mainMs / 1000)}
                        disabled={settingsLocked}
                        className={inputClassName}
                        onChange={(e) =>
                            onTimeSettingsChange({
                                ...timeSettings,
                                [side]: {
                                    ...settings,
                                    mainMs: Number(e.target.value) * 1000,
                                },
                            })
                        }
                    />
                </label>
                <label className={labelClassName}>
                    秒読み(秒)
                    <Input
                        type="number"
                        min={0}
                        value={Math.floor(settings.byoyomiMs / 1000)}
                        disabled={settingsLocked}
                        className={inputClassName}
                        onChange={(e) =>
                            onTimeSettingsChange({
                                ...timeSettings,
                                [side]: {
                                    ...settings,
                                    byoyomiMs: Number(e.target.value) * 1000,
                                },
                            })
                        }
                    />
                </label>
            </div>
        );
    };

    return (
        <div className="w-[var(--panel-width)] rounded-xl border border-[hsl(var(--border,0_0%_86%))] bg-[hsl(var(--card,0_0%_100%))] p-3 shadow-md">
            {/* 対局中のロックオーバーレイ */}
            {settingsLocked && (
                <div className="mb-2 flex items-center gap-2 rounded-lg bg-[hsl(var(--wafuu-sumi)/0.1)] px-3 py-1.5 text-xs text-muted-foreground">
                    <span>🔒</span>
                    <span>対局中は変更不可</span>
                </div>
            )}

            <div className="flex flex-col gap-3">
                {/* タイトル */}
                <div className="text-sm font-semibold text-[hsl(var(--wafuu-sumi))]">対局設定</div>

                {/* 手番設定 */}
                <label className={labelClassName}>
                    開始時の手番
                    <select
                        value={currentTurn}
                        onChange={(e) => onTurnChange(e.target.value as Player)}
                        disabled={settingsLocked}
                        className={selectClassName}
                    >
                        <option value="sente">先手から</option>
                        <option value="gote">後手から</option>
                    </select>
                </label>

                {/* 先手/後手設定（ヘッダー + エンジン + 持ち時間） */}
                <div className="grid grid-cols-2 gap-3">
                    {/* 先手側 */}
                    <div className="flex flex-col gap-3 border-r-2 border-[hsl(var(--wafuu-sumi)/0.2)] pr-3">
                        <div className="text-xs font-semibold text-wafuu-shu">☗先手</div>
                        {sideSelector("sente")}
                        {timeSelector("sente")}
                    </div>
                    {/* 後手側 */}
                    <div className="flex flex-col gap-3">
                        <div className="text-xs font-semibold text-wafuu-ai">☖後手</div>
                        {sideSelector("gote")}
                        {timeSelector("gote")}
                    </div>
                </div>
            </div>
        </div>
    );
}
