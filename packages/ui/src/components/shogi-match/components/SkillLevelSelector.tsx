import {
    detectSkillPreset,
    SKILL_LEVEL_MAX,
    SKILL_PRESET_LABELS,
    SKILL_PRESETS,
    type SkillLevelSettings,
    type SkillPreset,
} from "@shogi/engine-client";
import { type ReactElement, useMemo } from "react";

interface SkillLevelSelectorProps {
    /** 現在の設定値（undefined = デフォルト（全力）） */
    value: SkillLevelSettings | undefined;
    /** 設定変更時のコールバック */
    onChange: (settings: SkillLevelSettings | undefined) => void;
    /** 無効化 */
    disabled?: boolean;
}

const selectClassName =
    "p-2 rounded-lg border border-[hsl(var(--wafuu-border))] bg-[hsl(var(--card,0_0%_100%))]";
const labelClassName = "flex flex-col gap-1 text-[13px]";

/**
 * エンジンの強さ（Skill Level）を選択するコンポーネント
 */
export function SkillLevelSelector({
    value,
    onChange,
    disabled,
}: SkillLevelSelectorProps): ReactElement {
    // value から preset を派生（状態ではなく計算値）
    const preset = useMemo<SkillPreset>(() => {
        if (!value) return "professional";
        return detectSkillPreset(value);
    }, [value]);

    const handlePresetChange = (newPreset: SkillPreset) => {
        if (newPreset === "custom") {
            // カスタムに切り替え時は現在の値を維持
            // 全力（professional）から切り替えた場合は 20 から開始
            const defaultSkillLevel = preset === "professional" ? SKILL_LEVEL_MAX : 10;
            onChange(value ?? { skillLevel: defaultSkillLevel });
        } else if (newPreset === "professional") {
            // 全力の場合は undefined（デフォルト）を設定
            onChange(undefined);
        } else {
            // プリセット値を適用
            onChange(SKILL_PRESETS[newPreset]);
        }
    };

    const handleSkillLevelChange = (skillLevel: number) => {
        // 明示的にプロパティを設定（value が undefined でも安全）
        onChange({ skillLevel });
    };

    return (
        <div className="space-y-2">
            {/* プリセット選択 */}
            <label className={labelClassName}>
                エンジンの強さ
                <select
                    value={preset}
                    onChange={(e) => handlePresetChange(e.target.value as SkillPreset)}
                    disabled={disabled}
                    className={selectClassName}
                >
                    {(Object.keys(SKILL_PRESET_LABELS) as SkillPreset[]).map((p) => (
                        <option key={p} value={p}>
                            {SKILL_PRESET_LABELS[p]}
                        </option>
                    ))}
                </select>
            </label>

            {/* カスタム設定（preset="custom"時のみ表示） */}
            {preset === "custom" && (
                <div className="space-y-2 rounded-lg border border-[hsl(var(--wafuu-border))] bg-[hsl(var(--card,0_0%_100%)/0.5)] p-3">
                    {/* スキルレベルスライダー */}
                    <label className={labelClassName}>
                        <span className="flex justify-between">
                            <span>スキルレベル</span>
                            <span className="font-mono text-[hsl(var(--wafuu-kincha))]">
                                {value?.skillLevel ?? SKILL_LEVEL_MAX}
                            </span>
                        </span>
                        <input
                            type="range"
                            min={0}
                            max={20}
                            value={value?.skillLevel ?? SKILL_LEVEL_MAX}
                            onChange={(e) => handleSkillLevelChange(Number(e.target.value))}
                            disabled={disabled}
                            className="w-full accent-[hsl(var(--wafuu-shu))]"
                        />
                        <span className="flex justify-between text-[11px] text-[hsl(var(--muted-foreground))]">
                            <span>弱い</span>
                            <span>強い</span>
                        </span>
                    </label>
                </div>
            )}
        </div>
    );
}
