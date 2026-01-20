import { SKILL_LEVEL_MAX, type SkillLevelSettings } from "@shogi/engine-client";
import type { ReactElement } from "react";

interface SkillLevelSelectorProps {
    /** 現在の設定値（undefined = デフォルト（全力）） */
    value: SkillLevelSettings | undefined;
    /** 設定変更時のコールバック */
    onChange: (settings: SkillLevelSettings | undefined) => void;
    /** 無効化 */
    disabled?: boolean;
}

const labelClassName = "flex flex-col gap-1 text-[13px]";

/**
 * エンジンの強さ（レベル）を選択するコンポーネント
 */
export function SkillLevelSelector({
    value,
    onChange,
    disabled,
}: SkillLevelSelectorProps): ReactElement {
    // 現在のレベル（0-20、undefined時は最大値）
    const currentLevel = value?.skillLevel ?? SKILL_LEVEL_MAX;

    const handleLevelChange = (level: number) => {
        if (level === SKILL_LEVEL_MAX) {
            // 最大レベル（全力）の場合は undefined を設定
            onChange(undefined);
        } else {
            onChange({ skillLevel: level });
        }
    };

    return (
        <label className={labelClassName}>
            <span className="flex justify-between">
                <span>エンジンの強さ</span>
                <span className="font-mono text-[hsl(var(--wafuu-kincha))]">
                    レベル {currentLevel}
                </span>
            </span>
            <input
                type="range"
                min={0}
                max={SKILL_LEVEL_MAX}
                value={currentLevel}
                onChange={(e) => handleLevelChange(Number(e.target.value))}
                disabled={disabled}
                className="w-full accent-[hsl(var(--wafuu-shu))]"
            />
            <span className="flex justify-between text-[11px] text-[hsl(var(--muted-foreground))]">
                <span>弱い</span>
                <span>強い</span>
            </span>
        </label>
    );
}
