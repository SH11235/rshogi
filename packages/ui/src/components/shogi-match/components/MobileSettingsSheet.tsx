import type { SkillLevelSettings } from "@shogi/engine-client";
import { type ReactElement, useEffect, useState } from "react";
import { Switch } from "../../switch";
import type { ClockSettings } from "../hooks/useClockManager";
import type { DisplaySettings, PassRightsSettings, SquareNotation } from "../types";
import type { EngineOption, SideSetting } from "./MatchSettingsPanel";
import { SkillLevelSelector } from "./SkillLevelSelector";

type SideKey = "sente" | "gote";

// =============================================================================
// NumericInput: 文字列ベースの数値入力コンポーネント
// =============================================================================

interface NumericInputProps {
    id: string;
    value: number;
    onChange: (value: number) => void;
    disabled?: boolean;
    min?: number;
    className?: string;
}

/**
 * 編集中は空欄を許容し、blur時に数値変換する入力コンポーネント
 * - type="text" + inputMode="numeric" でモバイルで数字キーボードを表示
 * - 「0を消して3を入力」のような自然な操作が可能
 */
function NumericInput({
    id,
    value,
    onChange,
    disabled = false,
    min = 0,
    className,
}: NumericInputProps): ReactElement {
    const [inputValue, setInputValue] = useState(String(value));

    // 外部からの値変更を反映（ただし編集中でない場合のみ）
    useEffect(() => {
        setInputValue(String(value));
    }, [value]);

    const handleBlur = () => {
        // 空文字や無効な値は min に正規化
        const parsed = parseInt(inputValue, 10);
        const normalized = Number.isNaN(parsed) ? min : Math.max(min, parsed);
        setInputValue(String(normalized));
        onChange(normalized);
    };

    return (
        <input
            id={id}
            type="text"
            inputMode="numeric"
            pattern="[0-9]*"
            value={inputValue}
            disabled={disabled}
            className={`${className} h-10 rounded-md px-3 py-2`}
            onChange={(e) => setInputValue(e.target.value)}
            onBlur={handleBlur}
        />
    );
}

interface MobileSettingsSheetProps {
    // 対局設定
    sides: { sente: SideSetting; gote: SideSetting };
    onSidesChange: (sides: { sente: SideSetting; gote: SideSetting }) => void;
    timeSettings: ClockSettings;
    onTimeSettingsChange: (settings: ClockSettings) => void;

    // パス権設定（オプション）
    passRightsSettings?: PassRightsSettings;
    onPassRightsSettingsChange?: (settings: PassRightsSettings) => void;

    // エンジン情報
    uiEngineOptions: EngineOption[];

    // 状態
    settingsLocked: boolean;
    isMatchRunning: boolean;

    // アクション
    onStartMatch?: () => void;
    onStopMatch?: () => void;
    onResetToStartpos?: () => void;

    // 表示設定
    displaySettings: DisplaySettings;
    onDisplaySettingsChange: (settings: DisplaySettings) => void;
}

// iOS Safari は16px未満のinput/selectにフォーカスすると自動ズームするため、text-base(16px)を使用
const selectClassName = "w-full p-2 rounded-lg border border-border bg-background text-base";
const inputClassName = "w-full border border-border bg-background text-base";
const labelClassName = "flex flex-col gap-1 text-sm";

// SkillLevelSelectorの高さを確保してレイアウトシフトを防止
const SKILL_LEVEL_SELECTOR_MIN_HEIGHT = "min-h-[4rem]";

/**
 * モバイル用設定シート（BottomSheet内のコンテンツ）
 */
const NOTATION_OPTIONS: { value: SquareNotation; label: string }[] = [
    { value: "none", label: "非表示" },
    { value: "sfen", label: "SFEN (5e)" },
    { value: "japanese", label: "日本式 (５五)" },
];

export function MobileSettingsSheet({
    sides,
    onSidesChange,
    timeSettings,
    onTimeSettingsChange,
    passRightsSettings,
    onPassRightsSettingsChange,
    uiEngineOptions,
    settingsLocked,
    isMatchRunning,
    onStartMatch,
    onStopMatch,
    onResetToStartpos,
    displaySettings,
    onDisplaySettingsChange,
}: MobileSettingsSheetProps): ReactElement {
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

    return (
        <div className="flex flex-col gap-4 w-full max-w-full overflow-hidden">
            {/* 対局中のロック表示 */}
            {settingsLocked && (
                <div className="flex items-center gap-2 p-2 rounded-lg bg-destructive/10 text-destructive text-sm">
                    <span>対局中は設定を変更できません</span>
                </div>
            )}

            {/* 先手/後手ラベル + 入替ボタン */}
            <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-1 mb-1">
                <div className="text-sm font-semibold text-wafuu-shu text-center">☗先手</div>
                <button
                    type="button"
                    onClick={() => {
                        onSidesChange({ sente: sides.gote, gote: sides.sente });
                        onTimeSettingsChange({
                            sente: timeSettings.gote,
                            gote: timeSettings.sente,
                        });
                    }}
                    disabled={settingsLocked}
                    title="先手と後手の設定を入れ替える"
                    className="px-2 py-1 text-base text-muted-foreground hover:text-primary hover:bg-primary/10 rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                >
                    ⇄
                </button>
                <div className="text-sm font-semibold text-wafuu-ai text-center">☖後手</div>
            </div>
            {/* 先手/後手設定（PC版と同じ2列レイアウト） */}
            <div className="grid grid-cols-2 gap-3 [&>div]:min-w-0">
                {/* 先手側 */}
                <div className="flex flex-col gap-2 border-r border-border pr-3">
                    <label className={labelClassName}>
                        <span className="text-xs text-muted-foreground">プレイヤー</span>
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
                    {/* レイアウトシフト防止のため固定高さを確保 */}
                    <div className={SKILL_LEVEL_SELECTOR_MIN_HEIGHT}>
                        {sides.sente.role === "engine" && (
                            <SkillLevelSelector
                                value={sides.sente.skillLevel}
                                onChange={(sl) => handleSkillLevelChange("sente", sl)}
                                disabled={settingsLocked}
                            />
                        )}
                    </div>
                    <label htmlFor="mobile-sente-main" className={labelClassName}>
                        <span className="text-xs text-muted-foreground">持ち時間(秒)</span>
                        <NumericInput
                            id="mobile-sente-main"
                            value={Math.floor(timeSettings.sente.mainMs / 1000)}
                            disabled={settingsLocked}
                            className={inputClassName}
                            onChange={(v) =>
                                onTimeSettingsChange({
                                    ...timeSettings,
                                    sente: { ...timeSettings.sente, mainMs: v * 1000 },
                                })
                            }
                        />
                    </label>
                    <label htmlFor="mobile-sente-byoyomi" className={labelClassName}>
                        <span className="text-xs text-muted-foreground">秒読み(秒)</span>
                        <NumericInput
                            id="mobile-sente-byoyomi"
                            value={Math.floor(timeSettings.sente.byoyomiMs / 1000)}
                            disabled={settingsLocked}
                            className={inputClassName}
                            onChange={(v) =>
                                onTimeSettingsChange({
                                    ...timeSettings,
                                    sente: { ...timeSettings.sente, byoyomiMs: v * 1000 },
                                })
                            }
                        />
                    </label>
                </div>
                {/* 後手側 */}
                <div className="flex flex-col gap-2">
                    <label className={labelClassName}>
                        <span className="text-xs text-muted-foreground">プレイヤー</span>
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
                    {/* レイアウトシフト防止のため固定高さを確保 */}
                    <div className={SKILL_LEVEL_SELECTOR_MIN_HEIGHT}>
                        {sides.gote.role === "engine" && (
                            <SkillLevelSelector
                                value={sides.gote.skillLevel}
                                onChange={(sl) => handleSkillLevelChange("gote", sl)}
                                disabled={settingsLocked}
                            />
                        )}
                    </div>
                    <label htmlFor="mobile-gote-main" className={labelClassName}>
                        <span className="text-xs text-muted-foreground">持ち時間(秒)</span>
                        <NumericInput
                            id="mobile-gote-main"
                            value={Math.floor(timeSettings.gote.mainMs / 1000)}
                            disabled={settingsLocked}
                            className={inputClassName}
                            onChange={(v) =>
                                onTimeSettingsChange({
                                    ...timeSettings,
                                    gote: { ...timeSettings.gote, mainMs: v * 1000 },
                                })
                            }
                        />
                    </label>
                    <label htmlFor="mobile-gote-byoyomi" className={labelClassName}>
                        <span className="text-xs text-muted-foreground">秒読み(秒)</span>
                        <NumericInput
                            id="mobile-gote-byoyomi"
                            value={Math.floor(timeSettings.gote.byoyomiMs / 1000)}
                            disabled={settingsLocked}
                            className={inputClassName}
                            onChange={(v) =>
                                onTimeSettingsChange({
                                    ...timeSettings,
                                    gote: { ...timeSettings.gote, byoyomiMs: v * 1000 },
                                })
                            }
                        />
                    </label>
                </div>
            </div>

            {/* パス権設定（オプション） */}
            {passRightsSettings && onPassRightsSettingsChange && (
                <div className="space-y-3 pt-3 border-t border-border">
                    <div className="font-medium text-sm">変則ルール</div>
                    <div className="flex items-center justify-between">
                        <label htmlFor="mobile-pass-rights-toggle" className="text-sm">
                            パス権を有効にする
                        </label>
                        <Switch
                            id="mobile-pass-rights-toggle"
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
                        <div className="flex items-center justify-between">
                            <span className="text-sm text-muted-foreground">初期パス権数</span>
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
                                        settingsLocked || passRightsSettings.initialCount <= 0
                                    }
                                    className="flex h-8 w-8 items-center justify-center rounded border border-border bg-background text-base disabled:opacity-50"
                                >
                                    -
                                </button>
                                <span className="w-8 text-center text-base font-semibold">
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
                                        settingsLocked || passRightsSettings.initialCount >= 10
                                    }
                                    className="flex h-8 w-8 items-center justify-center rounded border border-border bg-background text-base disabled:opacity-50"
                                >
                                    +
                                </button>
                            </div>
                        </div>
                    )}
                    {passRightsSettings.enabled && (
                        <div className="flex items-center justify-between">
                            <span className="text-sm text-muted-foreground">
                                確認ダイアログしきい値(ms)
                            </span>
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
                                className="w-28 rounded border border-border bg-background px-2 py-1 text-sm"
                            />
                        </div>
                    )}
                    <p className="text-xs text-muted-foreground">
                        王手されていない時に手番をパスできます
                    </p>
                </div>
            )}

            {/* アクションボタン（頻繁に使うので上部に配置） */}
            <div className="flex justify-center gap-3 pt-3 border-t border-border">
                {isMatchRunning ? (
                    onStopMatch && (
                        <button
                            type="button"
                            onClick={onStopMatch}
                            className="flex-1 px-6 py-3 bg-destructive text-destructive-foreground rounded-lg font-medium shadow-md active:scale-95 transition-transform"
                        >
                            対局を停止
                        </button>
                    )
                ) : (
                    <>
                        {onResetToStartpos && (
                            <button
                                type="button"
                                onClick={onResetToStartpos}
                                className="px-4 py-3 border border-border rounded-lg font-medium hover:bg-muted active:scale-95 transition-all"
                            >
                                平手に戻す
                            </button>
                        )}
                        {onStartMatch && (
                            <button
                                type="button"
                                onClick={onStartMatch}
                                className="flex-1 px-6 py-3 bg-primary text-primary-foreground rounded-lg font-medium shadow-md active:scale-95 transition-transform"
                            >
                                対局を開始
                            </button>
                        )}
                    </>
                )}
            </div>

            {/* 表示設定 */}
            <div className="space-y-3 pt-3 border-t border-border">
                <div className="font-medium text-sm">表示設定</div>

                {/* 座標表示 */}
                <label htmlFor="mobile-notation" className={labelClassName}>
                    <span className="text-xs text-muted-foreground">座標表示</span>
                    <select
                        id="mobile-notation"
                        value={displaySettings.squareNotation}
                        onChange={(e) =>
                            onDisplaySettingsChange({
                                ...displaySettings,
                                squareNotation: e.target.value as SquareNotation,
                            })
                        }
                        className={selectClassName}
                    >
                        {NOTATION_OPTIONS.map((opt) => (
                            <option key={opt.value} value={opt.value}>
                                {opt.label}
                            </option>
                        ))}
                    </select>
                </label>

                {/* チェックボックス設定 */}
                <div className="space-y-2">
                    <label className="flex items-center gap-2 text-sm">
                        <input
                            type="checkbox"
                            checked={displaySettings.showBoardLabels}
                            onChange={(e) =>
                                onDisplaySettingsChange({
                                    ...displaySettings,
                                    showBoardLabels: e.target.checked,
                                })
                            }
                            className="w-4 h-4"
                        />
                        <span>盤外ラベル（筋・段）を表示</span>
                    </label>
                    <label className="flex items-center gap-2 text-sm">
                        <input
                            type="checkbox"
                            checked={displaySettings.highlightLastMove}
                            onChange={(e) =>
                                onDisplaySettingsChange({
                                    ...displaySettings,
                                    highlightLastMove: e.target.checked,
                                })
                            }
                            className="w-4 h-4"
                        />
                        <span>最終手を強調表示</span>
                    </label>
                </div>
            </div>
        </div>
    );
}
