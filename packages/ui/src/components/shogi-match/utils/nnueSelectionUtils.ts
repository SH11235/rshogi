/**
 * NNUE選択関連のユーティリティ関数
 *
 * KifuPanel, MoveDetailWindow などで重複していた
 * NNUE選択肢の構築・変換ロジックを共通化
 */

import type { NnueMeta, NnueSelection, PresetConfig } from "@shogi/app-core";

/**
 * select要素のvalue値のプレフィックス
 */
export const NNUE_VALUE_PREFIX = {
    MATERIAL: "material",
    PRESET: "preset:",
    CUSTOM: "custom:",
} as const;

/**
 * NNUE選択肢の型
 */
export interface NnueOption {
    type: "preset" | "custom";
    key: string;
    label: string;
}

/**
 * プリセット＋カスタムNNUEから選択肢リストを構築
 */
export function buildNnueOptions(params: {
    presets: PresetConfig[] | undefined;
    nnueList: NnueMeta[] | undefined;
}): NnueOption[] {
    const { presets, nnueList } = params;
    const options: NnueOption[] = [];

    // プリセット一覧
    for (const preset of presets ?? []) {
        const isDownloaded = nnueList?.some(
            (n) => n.source === "preset" && n.presetKey === preset.presetKey,
        );
        options.push({
            type: "preset",
            key: preset.presetKey,
            label: isDownloaded ? preset.displayName : `${preset.displayName} (要DL)`,
        });
    }

    // カスタムNNUE（プリセット以外）
    for (const nnue of nnueList ?? []) {
        if (nnue.source !== "preset") {
            options.push({
                type: "custom",
                key: nnue.id,
                label: nnue.displayName,
            });
        }
    }

    return options;
}

/**
 * NnueSelection → select要素のvalue文字列に変換
 *
 * @example
 * toNnueSelectionValue({ presetKey: "ramu", nnueId: null }) // "preset:ramu"
 * toNnueSelectionValue({ presetKey: null, nnueId: "abc123" }) // "custom:abc123"
 * toNnueSelectionValue({ presetKey: null, nnueId: null }) // "material"
 * toNnueSelectionValue(undefined) // "material"
 */
export function toNnueSelectionValue(selection: NnueSelection | undefined): string {
    if (!selection) return NNUE_VALUE_PREFIX.MATERIAL;
    if (selection.presetKey) return `${NNUE_VALUE_PREFIX.PRESET}${selection.presetKey}`;
    if (selection.nnueId) return `${NNUE_VALUE_PREFIX.CUSTOM}${selection.nnueId}`;
    return NNUE_VALUE_PREFIX.MATERIAL;
}

/**
 * select要素のvalue文字列 → NnueSelectionに変換
 *
 * @example
 * parseNnueSelectionValue("preset:ramu") // { presetKey: "ramu", nnueId: null }
 * parseNnueSelectionValue("custom:abc123") // { presetKey: null, nnueId: "abc123" }
 * parseNnueSelectionValue("material") // { presetKey: null, nnueId: null }
 */
export function parseNnueSelectionValue(value: string): NnueSelection {
    if (value.startsWith(NNUE_VALUE_PREFIX.PRESET)) {
        return {
            presetKey: value.slice(NNUE_VALUE_PREFIX.PRESET.length),
            nnueId: null,
        };
    }
    if (value.startsWith(NNUE_VALUE_PREFIX.CUSTOM)) {
        return {
            presetKey: null,
            nnueId: value.slice(NNUE_VALUE_PREFIX.CUSTOM.length),
        };
    }
    // "material" またはその他の値
    return {
        presetKey: null,
        nnueId: null,
    };
}

/**
 * NnueOption → option要素のvalue文字列に変換
 */
export function toOptionValue(option: NnueOption): string {
    return `${option.type}:${option.key}`;
}
