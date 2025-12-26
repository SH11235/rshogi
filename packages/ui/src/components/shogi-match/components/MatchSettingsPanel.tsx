import type { Player } from "@shogi/app-core";
import type { EngineClient } from "@shogi/engine-client";
import type { CSSProperties, ReactElement } from "react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../../collapsible";
import { Input } from "../../input";
import type { ClockSettings } from "../hooks/useClockManager";
import { formatTime } from "../utils/timeFormat";

const PANEL_STYLES = {
    select: {
        padding: "8px",
        borderRadius: "8px",
        border: "1px solid hsl(var(--wafuu-border))",
        background: "hsl(var(--card, 0 0% 100%))",
    } as CSSProperties,
    input: {
        border: "1px solid hsl(var(--wafuu-border))",
        background: "hsl(var(--card, 0 0% 100%))",
    } as CSSProperties,
    lockedOverlay: {
        position: "absolute",
        inset: 0,
        background: "hsl(var(--wafuu-washi-warm) / 0.7)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        borderRadius: "8px",
        zIndex: 10,
    } as CSSProperties,
    lockedIcon: {
        display: "flex",
        alignItems: "center",
        gap: "8px",
        padding: "8px 16px",
        background: "hsl(var(--wafuu-sumi) / 0.9)",
        color: "white",
        borderRadius: "8px",
        fontSize: "14px",
        fontWeight: 600,
    } as CSSProperties,
};

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
    // 折りたたみ時に表示するサマリー
    const getSideLabel = (setting: SideSetting): string => {
        if (setting.role === "human") return "人";
        const engine = uiEngineOptions.find((e) => e.id === setting.engineId);
        return engine?.label ?? "AI";
    };
    const getTimeSummary = (): string => {
        // 先手の設定を代表として表示（通常は先後同じ）
        const main = formatTime(timeSettings.sente.mainMs);
        const byoyomi = formatTime(timeSettings.sente.byoyomiMs);
        return `${main}+${byoyomi}`;
    };
    const summary = `☗${getSideLabel(sides.sente)} vs ☖${getSideLabel(sides.gote)} | ${getTimeSummary()}`;

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
            <label
                style={{
                    display: "flex",
                    flexDirection: "column",
                    gap: "4px",
                    fontSize: "13px",
                }}
            >
                {side === "sente" ? "先手" : "後手"}
                <select
                    value={selectorValue}
                    onChange={(e) => handleSelectorChange(side, e.target.value)}
                    disabled={settingsLocked}
                    style={PANEL_STYLES.select}
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
                        aria-label="対局設定パネルを開閉"
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
                                対局設定
                            </span>
                            {settingsLocked && (
                                <span
                                    title="対局中は変更できません"
                                    style={{
                                        fontSize: "16px",
                                        color: "hsl(var(--wafuu-shu))",
                                    }}
                                >
                                    🚫
                                </span>
                            )}
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
                            gap: "14px",
                            position: "relative",
                        }}
                    >
                        {/* 対局中のロックオーバーレイ */}
                        {settingsLocked && (
                            <div style={PANEL_STYLES.lockedOverlay}>
                                <div style={PANEL_STYLES.lockedIcon}>
                                    <span>🚫</span>
                                    <span>対局中は変更不可</span>
                                </div>
                            </div>
                        )}

                        <label
                            style={{
                                display: "flex",
                                flexDirection: "column",
                                gap: "4px",
                                fontSize: "13px",
                            }}
                        >
                            手番（開始時にどちらが指すか）
                            <select
                                value={currentTurn}
                                onChange={(e) => onTurnChange(e.target.value as Player)}
                                disabled={settingsLocked}
                                style={PANEL_STYLES.select}
                            >
                                <option value="sente">先手</option>
                                <option value="gote">後手</option>
                            </select>
                        </label>

                        <div
                            style={{
                                display: "grid",
                                gridTemplateColumns: "1fr 1fr",
                                gap: "12px",
                            }}
                        >
                            {sideSelector("sente")}
                            {sideSelector("gote")}
                        </div>

                        <div
                            style={{
                                display: "grid",
                                gridTemplateColumns: "1fr 1fr",
                                gap: "8px",
                            }}
                        >
                            <label
                                htmlFor="sente-main"
                                style={{
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: "4px",
                                    fontSize: "13px",
                                }}
                            >
                                先手 持ち時間 (ms)
                                <Input
                                    id="sente-main"
                                    type="number"
                                    value={timeSettings.sente.mainMs}
                                    disabled={settingsLocked}
                                    style={PANEL_STYLES.input}
                                    onChange={(e) =>
                                        onTimeSettingsChange({
                                            ...timeSettings,
                                            sente: {
                                                ...timeSettings.sente,
                                                mainMs: Number(e.target.value),
                                            },
                                        })
                                    }
                                />
                            </label>
                            <label
                                htmlFor="sente-byoyomi"
                                style={{
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: "4px",
                                    fontSize: "13px",
                                }}
                            >
                                先手 秒読み (ms)
                                <Input
                                    id="sente-byoyomi"
                                    type="number"
                                    value={timeSettings.sente.byoyomiMs}
                                    disabled={settingsLocked}
                                    style={PANEL_STYLES.input}
                                    onChange={(e) =>
                                        onTimeSettingsChange({
                                            ...timeSettings,
                                            sente: {
                                                ...timeSettings.sente,
                                                byoyomiMs: Number(e.target.value),
                                            },
                                        })
                                    }
                                />
                            </label>
                            <label
                                htmlFor="gote-main"
                                style={{
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: "4px",
                                    fontSize: "13px",
                                }}
                            >
                                後手 持ち時間 (ms)
                                <Input
                                    id="gote-main"
                                    type="number"
                                    value={timeSettings.gote.mainMs}
                                    disabled={settingsLocked}
                                    style={PANEL_STYLES.input}
                                    onChange={(e) =>
                                        onTimeSettingsChange({
                                            ...timeSettings,
                                            gote: {
                                                ...timeSettings.gote,
                                                mainMs: Number(e.target.value),
                                            },
                                        })
                                    }
                                />
                            </label>
                            <label
                                htmlFor="gote-byoyomi"
                                style={{
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: "4px",
                                    fontSize: "13px",
                                }}
                            >
                                後手 秒読み (ms)
                                <Input
                                    id="gote-byoyomi"
                                    type="number"
                                    value={timeSettings.gote.byoyomiMs}
                                    disabled={settingsLocked}
                                    style={PANEL_STYLES.input}
                                    onChange={(e) =>
                                        onTimeSettingsChange({
                                            ...timeSettings,
                                            gote: {
                                                ...timeSettings.gote,
                                                byoyomiMs: Number(e.target.value),
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
