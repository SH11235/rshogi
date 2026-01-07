import {
    detectSkillPreset,
    SKILL_LEVEL_MAX,
    SKILL_PRESET_LABELS,
    SKILL_PRESETS,
    type SkillLevelSettings,
    type SkillPreset,
} from "@shogi/engine-client";
import { type ReactElement, useEffect, useRef, useState } from "react";

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
    // UI上で選択されているプリセット
    // 「カスタム」を明示的に選択した場合、値がプリセットと一致してもカスタム表示を維持
    const [selectedPreset, setSelectedPreset] = useState<SkillPreset>(() => {
        if (!value) return "professional";
        return detectSkillPreset(value);
    });

    // 内部からの変更かどうかを追跡
    // 内部変更時は selectedPreset を維持、外部変更時は value から派生
    const isInternalChangeRef = useRef(false);

    useEffect(() => {
        if (isInternalChangeRef.current) {
            // 内部からの変更 - selectedPreset を維持
            isInternalChangeRef.current = false;
            return;
        }

        // 外部からの変更 - value から preset を派生
        const derivedPreset = value ? detectSkillPreset(value) : "professional";
        setSelectedPreset(derivedPreset);
    }, [value]);

    const handlePresetChange = (newPreset: SkillPreset) => {
        setSelectedPreset(newPreset);
        isInternalChangeRef.current = true;

        if (newPreset === "custom") {
            // カスタムに切り替え時は現在の値を維持（値は変更しない）
            const currentSkillLevel = value?.skillLevel ?? SKILL_LEVEL_MAX;
            onChange({ skillLevel: currentSkillLevel });
        } else if (newPreset === "professional") {
            // 全力の場合は undefined（デフォルト）を設定
            onChange(undefined);
        } else {
            // プリセット値を適用
            onChange(SKILL_PRESETS[newPreset]);
        }
    };

    const handleSkillLevelChange = (skillLevel: number) => {
        isInternalChangeRef.current = true;
        onChange({ skillLevel });
    };

    return (
        <div className="space-y-2">
            {/* プリセット選択 */}
            <label className={labelClassName}>
                エンジンの強さ
                <select
                    value={selectedPreset}
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

            {/* カスタム設定（selectedPreset="custom"時のみ表示） */}
            {selectedPreset === "custom" && (
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
