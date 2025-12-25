/**
 * 評価値グラフコンポーネント
 *
 * 横軸に手数、縦軸に評価値を表示する折れ線グラフ
 * 評価値の範囲に応じて自動スケール
 */

import type { CSSProperties, ReactElement } from "react";
import { useMemo } from "react";
import type { EvalHistory } from "../utils/kifFormat";

interface EvalGraphProps {
    /** 評価値の履歴 */
    evalHistory: EvalHistory[];
    /** 現在の手数（マーカー表示用） */
    currentPly: number;
    /** コンパクト表示（スマホ用） */
    compact?: boolean;
    /** グラフの高さ（px） */
    height?: number;
    /** 最小スケール範囲（センチポーン、デフォルト: 500 = ±5歩） */
    minScale?: number;
    /** クリック時のコールバック */
    onClick?: () => void;
}

/**
 * 評価値をY座標に変換（自動スケール対応版）
 */
function evalToYWithScale(
    evalCp: number | null | undefined,
    evalMate: number | null | undefined,
    height: number,
    scaleMax: number,
): number {
    const center = height / 2;
    const margin = 4;

    // 詰みの場合は上端または下端に固定
    if (evalMate !== undefined && evalMate !== null) {
        return evalMate > 0 ? margin : height - margin;
    }

    if (evalCp === undefined || evalCp === null) {
        return center; // 未計算は中央
    }

    // スケール範囲内で正規化
    const clamped = Math.max(-scaleMax, Math.min(scaleMax, evalCp));
    // 正の値は上（Y小）、負の値は下（Y大）
    const normalized = -clamped / scaleMax; // -1 ~ +1
    return center + normalized * (center - margin);
}

/**
 * 適切なスケール値を計算（きれいな数値に丸める）
 */
function computeNiceScale(maxAbsValue: number, minScale: number): number {
    // 最小スケールを保証
    const target = Math.max(maxAbsValue * 1.1, minScale); // 10%マージン

    // きれいな数値に丸める（100, 200, 500, 1000, 2000, 5000, 10000...）
    const niceValues = [100, 200, 500, 1000, 2000, 3000, 5000, 10000, 20000, 50000];
    for (const nice of niceValues) {
        if (nice >= target) {
            return nice;
        }
    }
    return Math.ceil(target / 10000) * 10000;
}

const containerStyle: CSSProperties = {
    position: "relative",
    width: "100%",
};

const yAxisLabelStyle: CSSProperties = {
    position: "absolute",
    left: 0,
    fontSize: "10px",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
    transform: "translateY(-50%)",
    width: "28px",
    textAlign: "right",
    paddingRight: "4px",
};

const xAxisContainerStyle: CSSProperties = {
    display: "flex",
    justifyContent: "space-between",
    marginLeft: "32px",
    marginTop: "2px",
    fontSize: "10px",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
};

/**
 * 評価値グラフ
 */
export function EvalGraph({
    evalHistory,
    currentPly,
    compact = false,
    height: customHeight,
    minScale = 500,
    onClick,
}: EvalGraphProps): ReactElement {
    const height = customHeight ?? (compact ? 60 : 80);
    const padding = { top: 4, bottom: 4, left: 0, right: 0 };
    const graphHeight = height - padding.top - padding.bottom;

    // 自動スケール計算
    const scaleMax = useMemo(() => {
        let maxAbs = 0;
        for (const entry of evalHistory) {
            if (entry.evalCp !== null && entry.evalCp !== undefined) {
                maxAbs = Math.max(maxAbs, Math.abs(entry.evalCp));
            }
            // 詰みの場合は大きな値として扱う
            if (entry.evalMate !== null && entry.evalMate !== undefined) {
                maxAbs = Math.max(maxAbs, 10000);
            }
        }
        return computeNiceScale(maxAbs, minScale);
    }, [evalHistory, minScale]);

    // ポイントの計算（null値を除外して連続した区間ごとにセグメントを生成）
    // viewBox="0 0 100 height" なので x は 0-100 の数値、y はピクセル値
    const lineSegments = useMemo(() => {
        if (evalHistory.length === 0) return [];

        const maxPly = Math.max(evalHistory.length - 1, 1);
        const segments: string[][] = [];
        let currentSegment: string[] = [];

        evalHistory.forEach((entry, index) => {
            const hasValue = entry.evalCp !== null || entry.evalMate !== null;
            if (hasValue) {
                const x = (index / maxPly) * 100;
                const y =
                    padding.top +
                    evalToYWithScale(entry.evalCp, entry.evalMate, graphHeight, scaleMax);
                currentSegment.push(`${x},${y}`);
            } else {
                // null値で区切り
                if (currentSegment.length > 0) {
                    segments.push(currentSegment);
                    currentSegment = [];
                }
            }
        });

        // 最後のセグメントを追加
        if (currentSegment.length > 0) {
            segments.push(currentSegment);
        }

        return segments;
    }, [evalHistory, graphHeight, scaleMax]);

    // ドット表示用のポイント（評価値がある手のみ）
    const dots = useMemo(() => {
        if (evalHistory.length === 0) return [];

        const maxPly = Math.max(evalHistory.length - 1, 1);

        return evalHistory
            .map((entry, index) => {
                const hasValue = entry.evalCp !== null || entry.evalMate !== null;
                if (!hasValue) return null;

                const x = (index / maxPly) * 100;
                const y =
                    padding.top +
                    evalToYWithScale(entry.evalCp, entry.evalMate, graphHeight, scaleMax);
                return { x, y, index };
            })
            .filter((dot): dot is { x: number; y: number; index: number } => dot !== null);
    }, [evalHistory, graphHeight, scaleMax]);

    // 現在位置のマーカー
    const currentMarker = useMemo(() => {
        if (currentPly < 0 || currentPly >= evalHistory.length) return null;

        const maxPly = Math.max(evalHistory.length - 1, 1);
        const entry = evalHistory[currentPly];
        const x = (currentPly / maxPly) * 100;
        const y =
            padding.top + evalToYWithScale(entry?.evalCp, entry?.evalMate, graphHeight, scaleMax);

        return { x, y };
    }, [currentPly, evalHistory, graphHeight, scaleMax]);

    // 塗りつぶし領域のパス（null値を除外）
    const fillPath = useMemo(() => {
        if (evalHistory.length === 0) return "";

        const maxPly = Math.max(evalHistory.length - 1, 1);
        const centerY = padding.top + graphHeight / 2;

        // 有効な値のみを抽出
        const validPoints = evalHistory
            .map((entry, index) => {
                const hasValue = entry.evalCp !== null || entry.evalMate !== null;
                if (!hasValue) return null;

                const x = (index / maxPly) * 100;
                const y =
                    padding.top +
                    evalToYWithScale(entry.evalCp, entry.evalMate, graphHeight, scaleMax);
                return { x, y };
            })
            .filter((p): p is { x: number; y: number } => p !== null);

        if (validPoints.length === 0) return "";

        // 上半分（先手有利）のパス
        const upperPath = validPoints
            .map((p, i) => {
                const y = Math.min(p.y, centerY);
                return `${i === 0 ? "M" : "L"} ${p.x} ${y}`;
            })
            .join(" ");

        // 最後の点から中央線へ、そして最初の点まで戻る
        const lastPoint = validPoints[validPoints.length - 1];
        const firstPoint = validPoints[0];
        const upperClose = `L ${lastPoint.x} ${centerY} L ${firstPoint.x} ${centerY} Z`;

        return upperPath + upperClose;
    }, [evalHistory, graphHeight, scaleMax]);

    // Y軸の目盛り値（センチポーン → 表示用）
    const yAxisLabels = useMemo(() => {
        const displayMax = scaleMax / 100;
        const halfValue = displayMax / 2;
        return [
            { value: `+${displayMax}`, position: padding.top },
            { value: `+${halfValue.toFixed(0)}`, position: padding.top + graphHeight / 4 },
            { value: "0", position: padding.top + graphHeight / 2 },
            { value: `-${halfValue.toFixed(0)}`, position: padding.top + (graphHeight * 3) / 4 },
            { value: `-${displayMax}`, position: padding.top + graphHeight },
        ];
    }, [scaleMax, graphHeight]);

    // X軸の目盛り（手数）
    const xAxisLabels = useMemo(() => {
        const maxPly = Math.max(evalHistory.length - 1, 1);
        if (maxPly <= 10) {
            return [0, maxPly];
        }
        if (maxPly <= 50) {
            const mid = Math.round(maxPly / 2);
            return [0, mid, maxPly];
        }
        // 50手以上の場合は4分割
        const quarter = Math.round(maxPly / 4);
        return [0, quarter, quarter * 2, quarter * 3, maxPly];
    }, [evalHistory.length]);

    if (compact) {
        // コンパクト表示（目盛り付き）
        return (
            // biome-ignore lint/a11y/noStaticElementInteractions: role/tabIndex are conditionally set when onClick is provided
            <div
                style={{ ...containerStyle, cursor: onClick ? "pointer" : undefined }}
                onClick={onClick}
                onKeyDown={onClick ? (e) => e.key === "Enter" && onClick() : undefined}
                role={onClick ? "button" : undefined}
                tabIndex={onClick ? 0 : undefined}
            >
                {/* Y軸ラベル */}
                {yAxisLabels
                    .filter((_, i) => i % 2 === 0) // 上・中・下の3つだけ表示
                    .map((label) => (
                        <span key={label.value} style={{ ...yAxisLabelStyle, top: label.position }}>
                            {label.value}
                        </span>
                    ))}

                {/* グラフ本体 */}
                <div style={{ marginLeft: "32px", height }}>
                    <svg
                        width="100%"
                        height={height}
                        style={{ display: "block" }}
                        viewBox={`0 0 100 ${height}`}
                        preserveAspectRatio="none"
                        role="img"
                        aria-label="評価値推移グラフ"
                    >
                        {/* 水平グリッド線 */}
                        {yAxisLabels.map((label) => (
                            <line
                                key={`grid-${label.value}`}
                                x1="0%"
                                y1={label.position}
                                x2="100%"
                                y2={label.position}
                                stroke="hsl(var(--border, 0 0% 86%))"
                                strokeWidth={label.value === "0" ? "1" : "0.5"}
                                vectorEffect="non-scaling-stroke"
                                strokeDasharray={label.value === "0" ? "none" : "2,2"}
                            />
                        ))}

                        {/* 塗りつぶし領域（先手有利部分） */}
                        {fillPath && (
                            <path
                                d={fillPath}
                                fill="hsl(var(--wafuu-shu, 350 80% 45%) / 0.15)"
                                stroke="none"
                            />
                        )}

                        {/* 評価値ライン（連続した区間ごとに描画） */}
                        {lineSegments.map((segment) => (
                            <polyline
                                key={`seg-${segment[0]}`}
                                points={segment.join(" ")}
                                fill="none"
                                stroke="hsl(var(--wafuu-shu, 350 80% 45%))"
                                strokeWidth="2"
                                vectorEffect="non-scaling-stroke"
                            />
                        ))}

                        {/* 各ポイントにドット表示 */}
                        {dots.map((dot) => (
                            <circle
                                key={`dot-${dot.index}`}
                                cx={dot.x}
                                cy={dot.y}
                                r="1.5"
                                fill="hsl(var(--wafuu-shu, 350 80% 45%))"
                            />
                        ))}

                        {/* 現在位置マーカー */}
                        {currentMarker && (
                            <circle
                                cx={currentMarker.x}
                                cy={currentMarker.y}
                                r="4"
                                fill="hsl(var(--primary, 210 100% 50%))"
                            />
                        )}
                    </svg>
                </div>

                {/* X軸ラベル（手数） */}
                <div style={xAxisContainerStyle}>
                    {xAxisLabels.map((ply) => (
                        <span key={ply}>{ply}手</span>
                    ))}
                </div>
            </div>
        );
    }

    // 通常表示
    const displayMax = scaleMax / 100;
    return (
        // biome-ignore lint/a11y/noStaticElementInteractions: role/tabIndex are conditionally set when onClick is provided
        <div
            className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)]"
            style={{ cursor: onClick ? "pointer" : undefined }}
            onClick={onClick}
            onKeyDown={onClick ? (e) => e.key === "Enter" && onClick() : undefined}
            role={onClick ? "button" : undefined}
            tabIndex={onClick ? 0 : undefined}
        >
            <div className="font-bold mb-1.5 text-sm">評価値推移</div>
            <div className="relative w-full" style={{ height }}>
                {/* 左側ラベル */}
                <span
                    className="absolute text-[10px] text-muted-foreground left-0 -translate-y-1/2"
                    style={{ top: padding.top }}
                >
                    +{displayMax}
                </span>
                <span
                    className="absolute text-[10px] text-muted-foreground left-0 -translate-y-1/2"
                    style={{ top: padding.top + graphHeight / 2 }}
                >
                    0
                </span>
                <span
                    className="absolute text-[10px] text-muted-foreground left-0 translate-y-1/2"
                    style={{ bottom: padding.bottom }}
                >
                    -{displayMax}
                </span>

                <svg
                    width="100%"
                    height={height}
                    className="block ml-5"
                    style={{ width: "calc(100% - 20px)" }}
                    viewBox={`0 0 100 ${height}`}
                    preserveAspectRatio="none"
                    role="img"
                    aria-label="評価値推移グラフ"
                >
                    {/* 背景グリッド */}
                    <line
                        x1="0%"
                        y1={padding.top}
                        x2="100%"
                        y2={padding.top}
                        stroke="hsl(var(--border))"
                        strokeWidth="0.5"
                        vectorEffect="non-scaling-stroke"
                        strokeDasharray="2,2"
                    />
                    <line
                        x1="0%"
                        y1={padding.top + graphHeight}
                        x2="100%"
                        y2={padding.top + graphHeight}
                        stroke="hsl(var(--border))"
                        strokeWidth="0.5"
                        vectorEffect="non-scaling-stroke"
                        strokeDasharray="2,2"
                    />

                    {/* 中央線（0評価） */}
                    <line
                        x1="0%"
                        y1={padding.top + graphHeight / 2}
                        x2="100%"
                        y2={padding.top + graphHeight / 2}
                        stroke="hsl(var(--border))"
                        strokeWidth="1"
                        vectorEffect="non-scaling-stroke"
                    />

                    {/* 塗りつぶし領域（先手有利部分） */}
                    {fillPath && (
                        <path d={fillPath} fill="hsl(var(--wafuu-shu) / 0.15)" stroke="none" />
                    )}

                    {/* 評価値ライン（連続した区間ごとに描画） */}
                    {lineSegments.map((segment) => (
                        <polyline
                            key={`seg-${segment[0]}`}
                            points={segment.join(" ")}
                            fill="none"
                            stroke="hsl(var(--wafuu-shu))"
                            strokeWidth="2"
                            vectorEffect="non-scaling-stroke"
                        />
                    ))}

                    {/* 各ポイントにドット表示 */}
                    {dots.map((dot) => (
                        <circle
                            key={`dot-${dot.index}`}
                            cx={dot.x}
                            cy={dot.y}
                            r="1.5"
                            fill="hsl(var(--wafuu-shu))"
                        />
                    ))}

                    {/* 現在位置マーカー */}
                    {currentMarker && (
                        <circle
                            cx={currentMarker.x}
                            cy={currentMarker.y}
                            r="4"
                            fill="hsl(var(--primary))"
                        />
                    )}
                </svg>
            </div>

            {/* 手数表示 */}
            <div className="flex justify-between mt-1 ml-5">
                <span className="text-[10px] text-muted-foreground">0</span>
                <span className="text-[10px] text-muted-foreground">
                    {Math.max(evalHistory.length - 1, 0)}手
                </span>
            </div>
        </div>
    );
}
