import type { EngineClient, SkillLevelSettings } from "@shogi/engine-client";
import type { ReactElement } from "react";
import { Input } from "../../input";
import { Switch } from "../../switch";
import type { ClockSettings } from "../hooks/useClockManager";
import type { PassRightsSettings } from "../types";
import { SkillLevelSelector } from "./SkillLevelSelector";

type SideKey = "sente" | "gote";

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

    // パス権設定（オプション）
    passRightsSettings?: PassRightsSettings;
    onPassRightsSettingsChange?: (settings: PassRightsSettings) => void;

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
    passRightsSettings,
    onPassRightsSettingsChange,
    uiEngineOptions,
    settingsLocked,
}: MatchSettingsPanelProps): ReactElement {
    // 選択肢の値を生成: "human" または "engine:{engineId}"
    const getSelectorValue = (setting: SideSetting): string => {
        if (setting.role === "human") return "human";
        return `engine:${setting.engineId ?? uiEngineOptions[0]?.id ?? ""}`;
    };

    const handleSelectorChange = (side: SideKey, value: string) => {
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

    const handleSkillLevelChange = (side: SideKey, skillLevel: SkillLevelSettings | undefined) => {
        onSidesChange({
            ...sides,
            [side]: { ...sides[side], skillLevel },
        });
    };

    const sideSelector = (side: SideKey) => {
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

    const timeSelector = (side: SideKey) => {
        const settings = timeSettings[side];
        // 最大24時間（86400秒）
        const MAX_SECONDS = 86400;

        const handleTimeChange = (field: "mainMs" | "byoyomiMs", inputValue: string) => {
            const parsed = Number(inputValue);
            // NaNまたは負の値は無視
            if (Number.isNaN(parsed) || parsed < 0) return;
            // 最大値でクランプ
            const clampedSeconds = Math.min(Math.floor(parsed), MAX_SECONDS);
            onTimeSettingsChange({
                ...timeSettings,
                [side]: {
                    ...settings,
                    [field]: clampedSeconds * 1000,
                },
            });
        };

        return (
            <div className="flex flex-col gap-1.5">
                {/* biome-ignore lint/a11y/noLabelWithoutControl: Input component renders native input inside label */}
                <label className={labelClassName}>
                    持ち時間(秒)
                    <Input
                        type="number"
                        min={0}
                        max={MAX_SECONDS}
                        value={Math.floor(settings.mainMs / 1000)}
                        disabled={settingsLocked}
                        className={inputClassName}
                        onChange={(e) => handleTimeChange("mainMs", e.target.value)}
                    />
                </label>
                {/* biome-ignore lint/a11y/noLabelWithoutControl: Input component renders native input inside label */}
                <label className={labelClassName}>
                    秒読み(秒)
                    <Input
                        type="number"
                        min={0}
                        max={MAX_SECONDS}
                        value={Math.floor(settings.byoyomiMs / 1000)}
                        disabled={settingsLocked}
                        className={inputClassName}
                        onChange={(e) => handleTimeChange("byoyomiMs", e.target.value)}
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

                {/* 先手/後手設定 */}
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

                {/* パス権設定（オプション） */}
                {passRightsSettings && onPassRightsSettingsChange && (
                    <>
                        <div className="h-px bg-[hsl(var(--border,0_0%_86%))]" />
                        <div className="flex flex-col gap-2">
                            <div className="text-xs font-semibold text-[hsl(var(--wafuu-sumi))]">
                                変則ルール
                            </div>
                            <div className="flex items-center justify-between">
                                <label
                                    htmlFor="pass-rights-toggle"
                                    className="text-xs text-muted-foreground"
                                >
                                    パス権を有効にする
                                </label>
                                <Switch
                                    id="pass-rights-toggle"
                                    checked={passRightsSettings.enabled}
                                    onCheckedChange={(checked) =>
                                        onPassRightsSettingsChange({
                                            ...passRightsSettings,
                                            enabled: checked,
                                        })
                                    }
                                    disabled={settingsLocked}
                                />
                            </div>
                            {passRightsSettings.enabled && (
                                <label className={labelClassName}>
                                    初期パス権数
                                    <div className="flex items-center gap-2">
                                        <button
                                            type="button"
                                            onClick={() =>
                                                onPassRightsSettingsChange({
                                                    ...passRightsSettings,
                                                    initialCount: Math.max(
                                                        0,
                                                        passRightsSettings.initialCount - 1,
                                                    ),
                                                })
                                            }
                                            disabled={
                                                settingsLocked ||
                                                passRightsSettings.initialCount <= 0
                                            }
                                            className="flex h-8 w-8 items-center justify-center rounded border border-[hsl(var(--border,0_0%_86%))] bg-[hsl(var(--card,0_0%_100%))] text-sm disabled:opacity-50"
                                        >
                                            -
                                        </button>
                                        <span className="w-8 text-center text-sm font-semibold">
                                            {passRightsSettings.initialCount}
                                        </span>
                                        <button
                                            type="button"
                                            onClick={() =>
                                                onPassRightsSettingsChange({
                                                    ...passRightsSettings,
                                                    initialCount: Math.min(
                                                        10,
                                                        passRightsSettings.initialCount + 1,
                                                    ),
                                                })
                                            }
                                            disabled={
                                                settingsLocked ||
                                                passRightsSettings.initialCount >= 10
                                            }
                                            className="flex h-8 w-8 items-center justify-center rounded border border-[hsl(var(--border,0_0%_86%))] bg-[hsl(var(--card,0_0%_100%))] text-sm disabled:opacity-50"
                                        >
                                            +
                                        </button>
                                    </div>
                                </label>
                            )}
                            {passRightsSettings.enabled && (
                                <label className={labelClassName}>
                                    パス確認ダイアログしきい値（ms）
                                    <div className="flex items-center gap-2">
                                        <input
                                            type="number"
                                            min={0}
                                            step={500}
                                            value={passRightsSettings.confirmDialogThresholdMs}
                                            onChange={(e) =>
                                                onPassRightsSettingsChange({
                                                    ...passRightsSettings,
                                                    confirmDialogThresholdMs: Math.max(
                                                        0,
                                                        Number(e.target.value) || 0,
                                                    ),
                                                })
                                            }
                                            disabled={settingsLocked}
                                            className="w-28 rounded border border-[hsl(var(--border,0_0%_86%))] bg-[hsl(var(--card,0_0%_100%))] px-2 py-1 text-sm"
                                        />
                                        <span className="text-xs text-muted-foreground">
                                            0で即時、時間が多ければ確認
                                        </span>
                                    </div>
                                </label>
                            )}
                            <p className="text-xs text-muted-foreground/70">
                                王手されていない時に手番をパスできます
                            </p>
                        </div>
                    </>
                )}
            </div>
        </div>
    );
}
