import {
    detectSkillPreset,
    SKILL_PRESET_LABELS,
    SKILL_PRESETS,
    type SkillLevelSettings,
    type SkillPreset,
} from "@shogi/engine-client";
import { type ReactElement, useState } from "react";

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
    // プリセット状態を管理（value から初期値を推定）
    const [preset, setPreset] = useState<SkillPreset>(() => {
        if (!value) return "professional";
        return detectSkillPreset(value);
    });

    const handlePresetChange = (newPreset: SkillPreset) => {
        setPreset(newPreset);

        if (newPreset === "custom") {
            // カスタムに切り替え時は現在の値を維持、なければデフォルト
            onChange(value ?? { skillLevel: 10 });
        } else if (newPreset === "professional") {
            // 全力の場合は undefined（デフォルト）を設定
            onChange(undefined);
        } else {
            // プリセット値を適用
            onChange(SKILL_PRESETS[newPreset]);
        }
    };

    const handleSkillLevelChange = (skillLevel: number) => {
        onChange({ ...value, skillLevel });
    };

    const handleEloToggle = (useLimitStrength: boolean) => {
        if (useLimitStrength) {
            onChange({
                ...value,
                skillLevel: value?.skillLevel ?? 10,
                useLimitStrength: true,
                elo: 1500,
            });
        } else {
            onChange({ skillLevel: value?.skillLevel ?? 10 });
        }
    };

    const handleEloChange = (elo: number) => {
        onChange({ ...value, skillLevel: value?.skillLevel ?? 10, useLimitStrength: true, elo });
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
                                {value?.skillLevel ?? 20}
                            </span>
                        </span>
                        <input
                            type="range"
                            min={0}
                            max={20}
                            value={value?.skillLevel ?? 20}
                            onChange={(e) => handleSkillLevelChange(Number(e.target.value))}
                            disabled={disabled}
                            className="w-full accent-[hsl(var(--wafuu-shu))]"
                        />
                        <span className="flex justify-between text-[11px] text-[hsl(var(--muted-foreground))]">
                            <span>弱い</span>
                            <span>強い</span>
                        </span>
                    </label>

                    {/* ELO指定オプション */}
                    <label className="flex cursor-pointer items-center gap-2 text-[13px]">
                        <input
                            type="checkbox"
                            checked={value?.useLimitStrength ?? false}
                            onChange={(e) => handleEloToggle(e.target.checked)}
                            disabled={disabled}
                            className="accent-[hsl(var(--wafuu-shu))]"
                        />
                        <span>ELOで指定</span>
                    </label>

                    {value?.useLimitStrength && (
                        <label className={labelClassName}>
                            <span className="flex justify-between">
                                <span>ELO</span>
                                <span className="font-mono text-[hsl(var(--wafuu-kincha))]">
                                    {value?.elo ?? 1500}
                                </span>
                            </span>
                            <input
                                type="range"
                                min={1320}
                                max={3190}
                                step={10}
                                value={value?.elo ?? 1500}
                                onChange={(e) => handleEloChange(Number(e.target.value))}
                                disabled={disabled}
                                className="w-full accent-[hsl(var(--wafuu-shu))]"
                            />
                            <span className="flex justify-between text-[11px] text-[hsl(var(--muted-foreground))]">
                                <span>1320</span>
                                <span>3190</span>
                            </span>
                        </label>
                    )}
                </div>
            )}
        </div>
    );
}
