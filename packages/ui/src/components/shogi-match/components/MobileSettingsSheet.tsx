import type { Player } from "@shogi/app-core";
import type { ReactElement } from "react";
import { Input } from "../../input";
import type { ClockSettings } from "../hooks/useClockManager";
import type { EngineOption, SideSetting } from "./MatchSettingsPanel";

interface MobileSettingsSheetProps {
    // 対局設定
    sides: { sente: SideSetting; gote: SideSetting };
    onSidesChange: (sides: { sente: SideSetting; gote: SideSetting }) => void;
    timeSettings: ClockSettings;
    onTimeSettingsChange: (settings: ClockSettings) => void;
    currentTurn: Player;
    onTurnChange: (turn: Player) => void;

    // エンジン情報
    uiEngineOptions: EngineOption[];

    // 状態
    settingsLocked: boolean;
    isMatchRunning: boolean;

    // アクション
    onStartMatch?: () => void;
    onStopMatch?: () => void;
}

const selectClassName = "w-full p-2 rounded-lg border border-border bg-background text-sm";
const inputClassName = "w-full border border-border bg-background text-sm";
const labelClassName = "flex flex-col gap-1 text-sm";

/**
 * モバイル用設定シート（BottomSheet内のコンテンツ）
 */
export function MobileSettingsSheet({
    sides,
    onSidesChange,
    timeSettings,
    onTimeSettingsChange,
    currentTurn,
    onTurnChange,
    uiEngineOptions,
    settingsLocked,
    isMatchRunning,
    onStartMatch,
    onStopMatch,
}: MobileSettingsSheetProps): ReactElement {
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

    return (
        <div className="flex flex-col gap-4">
            {/* 対局中のロック表示 */}
            {settingsLocked && (
                <div className="flex items-center gap-2 p-2 rounded-lg bg-destructive/10 text-destructive text-sm">
                    <span>対局中は設定を変更できません</span>
                </div>
            )}

            {/* 手番設定 */}
            <label className={labelClassName}>
                <span className="font-medium">手番（開始時）</span>
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

            {/* 先手/後手設定 */}
            <div className="grid grid-cols-2 gap-3">
                <label className={labelClassName}>
                    <span className="font-medium text-wafuu-shu">☗ 先手</span>
                    <select
                        value={getSelectorValue(sides.sente)}
                        onChange={(e) => handleSelectorChange("sente", e.target.value)}
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
                <label className={labelClassName}>
                    <span className="font-medium text-wafuu-ai">☖ 後手</span>
                    <select
                        value={getSelectorValue(sides.gote)}
                        onChange={(e) => handleSelectorChange("gote", e.target.value)}
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
            </div>

            {/* 持ち時間設定 */}
            <div className="space-y-2">
                <div className="font-medium text-sm">持ち時間</div>
                <div className="grid grid-cols-2 gap-2">
                    <label htmlFor="mobile-sente-main" className={labelClassName}>
                        <span className="text-xs text-muted-foreground">先手 持ち時間(秒)</span>
                        <Input
                            id="mobile-sente-main"
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
                    <label htmlFor="mobile-sente-byoyomi" className={labelClassName}>
                        <span className="text-xs text-muted-foreground">先手 秒読み(秒)</span>
                        <Input
                            id="mobile-sente-byoyomi"
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
                    <label htmlFor="mobile-gote-main" className={labelClassName}>
                        <span className="text-xs text-muted-foreground">後手 持ち時間(秒)</span>
                        <Input
                            id="mobile-gote-main"
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
                    <label htmlFor="mobile-gote-byoyomi" className={labelClassName}>
                        <span className="text-xs text-muted-foreground">後手 秒読み(秒)</span>
                        <Input
                            id="mobile-gote-byoyomi"
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

            {/* アクションボタン */}
            <div className="flex justify-center gap-3 pt-2 border-t border-border">
                {isMatchRunning
                    ? onStopMatch && (
                          <button
                              type="button"
                              onClick={onStopMatch}
                              className="flex-1 px-6 py-3 bg-destructive text-destructive-foreground rounded-lg font-medium shadow-md active:scale-95 transition-transform"
                          >
                              対局を停止
                          </button>
                      )
                    : onStartMatch && (
                          <button
                              type="button"
                              onClick={onStartMatch}
                              className="flex-1 px-6 py-3 bg-primary text-primary-foreground rounded-lg font-medium shadow-md active:scale-95 transition-transform"
                          >
                              対局を開始
                          </button>
                      )}
            </div>
        </div>
    );
}
