import type { Player } from "@shogi/app-core";
import type { EngineClient } from "@shogi/engine-client";
import type { ReactElement } from "react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../../collapsible";
import { Input } from "../../input";
import type { ClockSettings } from "../hooks/useClockManager";

type SideRole = "human" | "engine";

export type SideSetting = {
    role: SideRole;
    engineId?: string;
};

export type EngineOption = {
    id: string;
    label: string;
    createClient: () => EngineClient;
    kind?: "internal" | "external";
};

interface MatchSettingsPanelProps {
    // パネル表示状態
    isOpen: boolean;
    onOpenChange: (open: boolean) => void;

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
    "p-2 rounded-lg border border-[hsl(var(--wafuu-border))] bg-[hsl(var(--card,0_0%_100%))]";
const inputClassName = "border border-[hsl(var(--wafuu-border))] bg-[hsl(var(--card,0_0%_100%))]";
const labelClassName = "flex flex-col gap-1 text-[13px]";

export function MatchSettingsPanel({
    isOpen,
    onOpenChange,
    sides,
    onSidesChange,
    timeSettings,
    onTimeSettingsChange,
    currentTurn,
    onTurnChange,
    uiEngineOptions,
    settingsLocked,
}: MatchSettingsPanelProps): ReactElement {
    // 折りたたみ時に表示するサマリー（短いラベル）
    const getSideLabel = (setting: SideSetting): string => {
        return setting.role === "human" ? "人" : "AI";
    };
    const summary = `☗${getSideLabel(sides.sente)} vs ☖${getSideLabel(sides.gote)}`;

    // 選択肢の値を生成: "human" または "engine:{engineId}"
    const getSelectorValue = (setting: SideSetting): string => {
        if (setting.role === "human") return "human";
        return `engine:${setting.engineId ?? uiEngineOptions[0]?.id ?? ""}`;
    };

    const handleSelectorChange = (side: Player, value: string) => {
        if (value === "human") {
            onSidesChange({
                ...sides,
                [side]: { role: "human", engineId: undefined },
            });
        } else if (value.startsWith("engine:")) {
            const engineId = value.slice("engine:".length);
            onSidesChange({
                ...sides,
                [side]: { role: "engine", engineId },
            });
        }
    };

    const sideSelector = (side: Player) => {
        const setting = sides[side];
        const selectorValue = getSelectorValue(setting);

        return (
            <label className={labelClassName}>
                {side === "sente" ? "先手" : "後手"}
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
        );
    };

    return (
        <Collapsible open={isOpen} onOpenChange={onOpenChange}>
            <div className="w-[var(--panel-width)] overflow-hidden rounded-xl border-2 border-[hsl(var(--wafuu-border))] bg-[hsl(var(--wafuu-washi-warm))] shadow-lg">
                <CollapsibleTrigger asChild>
                    <button
                        type="button"
                        aria-label="対局設定パネルを開閉"
                        className={`flex w-full cursor-pointer items-center justify-between gap-3 border-none bg-gradient-to-br from-[hsl(var(--wafuu-washi))] to-[hsl(var(--wafuu-washi-warm))] px-4 py-3.5 transition-all duration-200 ${
                            isOpen ? "border-b border-[hsl(var(--wafuu-border))]" : ""
                        }`}
                    >
                        <span className="flex items-center gap-3">
                            <span className="text-lg font-bold tracking-wide text-[hsl(var(--wafuu-sumi))]">
                                対局設定
                            </span>
                            {settingsLocked && (
                                <span
                                    title="対局中は変更できません"
                                    className="text-base text-[hsl(var(--wafuu-shu))]"
                                >
                                    🚫
                                </span>
                            )}
                            <span className="text-sm font-semibold text-[hsl(var(--wafuu-kincha))]">
                                {summary}
                            </span>
                        </span>
                        <span
                            className={`shrink-0 text-xl text-[hsl(var(--wafuu-kincha))] transition-transform duration-200 ${
                                isOpen ? "rotate-180" : "rotate-0"
                            }`}
                        >
                            ▼
                        </span>
                    </button>
                </CollapsibleTrigger>
                <CollapsibleContent>
                    <div className="relative flex flex-col gap-3.5 p-4">
                        {/* 対局中のロックオーバーレイ */}
                        {settingsLocked && (
                            <div className="absolute inset-0 z-10 flex items-center justify-center rounded-lg bg-[hsl(var(--wafuu-washi-warm)/0.7)]">
                                <div className="flex items-center gap-2 rounded-lg bg-[hsl(var(--wafuu-sumi)/0.9)] px-4 py-2 text-sm font-semibold text-white">
                                    <span>🚫</span>
                                    <span>対局中は変更不可</span>
                                </div>
                            </div>
                        )}

                        <label className={labelClassName}>
                            手番（開始時にどちらが指すか）
                            <select
                                value={currentTurn}
                                onChange={(e) => onTurnChange(e.target.value as Player)}
                                disabled={settingsLocked}
                                className={selectClassName}
                            >
                                <option value="sente">先手</option>
                                <option value="gote">後手</option>
                            </select>
                        </label>

                        <div className="grid grid-cols-2 gap-3">
                            {sideSelector("sente")}
                            {sideSelector("gote")}
                        </div>

                        <div className="grid grid-cols-2 gap-2">
                            <label htmlFor="sente-main" className={labelClassName}>
                                先手 持ち時間 (秒)
                                <Input
                                    id="sente-main"
                                    type="number"
                                    min={0}
                                    value={Math.floor(timeSettings.sente.mainMs / 1000)}
                                    disabled={settingsLocked}
                                    className={inputClassName}
                                    onChange={(e) =>
                                        onTimeSettingsChange({
                                            ...timeSettings,
                                            sente: {
                                                ...timeSettings.sente,
                                                mainMs: Number(e.target.value) * 1000,
                                            },
                                        })
                                    }
                                />
                            </label>
                            <label htmlFor="sente-byoyomi" className={labelClassName}>
                                先手 秒読み (秒)
                                <Input
                                    id="sente-byoyomi"
                                    type="number"
                                    min={0}
                                    value={Math.floor(timeSettings.sente.byoyomiMs / 1000)}
                                    disabled={settingsLocked}
                                    className={inputClassName}
                                    onChange={(e) =>
                                        onTimeSettingsChange({
                                            ...timeSettings,
                                            sente: {
                                                ...timeSettings.sente,
                                                byoyomiMs: Number(e.target.value) * 1000,
                                            },
                                        })
                                    }
                                />
                            </label>
                            <label htmlFor="gote-main" className={labelClassName}>
                                後手 持ち時間 (秒)
                                <Input
                                    id="gote-main"
                                    type="number"
                                    min={0}
                                    value={Math.floor(timeSettings.gote.mainMs / 1000)}
                                    disabled={settingsLocked}
                                    className={inputClassName}
                                    onChange={(e) =>
                                        onTimeSettingsChange({
                                            ...timeSettings,
                                            gote: {
                                                ...timeSettings.gote,
                                                mainMs: Number(e.target.value) * 1000,
                                            },
                                        })
                                    }
                                />
                            </label>
                            <label htmlFor="gote-byoyomi" className={labelClassName}>
                                後手 秒読み (秒)
                                <Input
                                    id="gote-byoyomi"
                                    type="number"
                                    min={0}
                                    value={Math.floor(timeSettings.gote.byoyomiMs / 1000)}
                                    disabled={settingsLocked}
                                    className={inputClassName}
                                    onChange={(e) =>
                                        onTimeSettingsChange({
                                            ...timeSettings,
                                            gote: {
                                                ...timeSettings.gote,
                                                byoyomiMs: Number(e.target.value) * 1000,
                                            },
                                        })
                                    }
                                />
                            </label>
                        </div>
                    </div>
                </CollapsibleContent>
            </div>
        </Collapsible>
    );
}
