/**
 * 評価値グラフコンポーネント
 *
 * 横軸に手数、縦軸に評価値を表示する折れ線グラフ
 * 評価値の範囲に応じて自動スケール
 */

import type { MouseEvent, ReactElement } from "react";
import { useCallback, useMemo, useRef } from "react";
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
    /** クリック時のコールバック（既存の拡大表示用） */
    onClick?: () => void;
    /** 手数選択時のコールバック */
    onPlySelect?: (ply: number) => void;
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
    onPlySelect,
}: EvalGraphProps): ReactElement {
    const height = customHeight ?? (compact ? 60 : 80);
    const padding = { top: 4, bottom: 4, left: 0, right: 0 };
    const graphHeight = height - padding.top - padding.bottom;
    const graphContainerRef = useRef<HTMLElement>(null);

    // グラフクリック時に手数を計算
    const handleGraphClick = useCallback(
        (e: MouseEvent<HTMLDivElement | HTMLButtonElement>) => {
            if (!onPlySelect || evalHistory.length <= 1) return;

            const container = graphContainerRef.current;
            if (!container) return;

            const rect = container.getBoundingClientRect();
            const x = e.clientX - rect.left;
            const width = rect.width;

            // クリック位置から手数を計算
            const maxPly = evalHistory.length - 1;
            const clickedPly = Math.round((x / width) * maxPly);
            const clampedPly = Math.max(0, Math.min(maxPly, clickedPly));

            onPlySelect(clampedPly);
        },
        [onPlySelect, evalHistory.length],
    );

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

    // ポイントの計算（先手・後手を分離して別々のセグメントを生成）
    // viewBox="0 0 100 height" なので x は 0-100 の数値、y はピクセル値
    const { senteSegments, goteSegments } = useMemo(() => {
        if (evalHistory.length === 0) return { senteSegments: [], goteSegments: [] };

        const maxPly = Math.max(evalHistory.length - 1, 1);
        const sente: string[][] = [];
        const gote: string[][] = [];
        let currentSenteSegment: string[] = [];
        let currentGoteSegment: string[] = [];

        evalHistory.forEach((entry, index) => {
            const hasValue = entry.evalCp !== null || entry.evalMate !== null;
            // ply=0は初期局面（評価値0）、ply奇数は先手の手、ply偶数は後手の手
            const isSenteMove = entry.ply % 2 === 1;

            if (hasValue) {
                const x = (index / maxPly) * 100;
                const y =
                    padding.top +
                    evalToYWithScale(entry.evalCp, entry.evalMate, graphHeight, scaleMax);
                const point = `${x},${y}`;

                if (index === 0) {
                    // 初期局面（ply=0）は両方に追加
                    currentSenteSegment.push(point);
                    currentGoteSegment.push(point);
                } else if (isSenteMove) {
                    currentSenteSegment.push(point);
                } else {
                    currentGoteSegment.push(point);
                }
            } else {
                // null値で区切り
                if (index === 0 || entry.ply % 2 === 1) {
                    if (currentSenteSegment.length > 0) {
                        sente.push(currentSenteSegment);
                        currentSenteSegment = [];
                    }
                }
                if (index === 0 || entry.ply % 2 === 0) {
                    if (currentGoteSegment.length > 0) {
                        gote.push(currentGoteSegment);
                        currentGoteSegment = [];
                    }
                }
            }
        });

        // 最後のセグメントを追加
        if (currentSenteSegment.length > 0) {
            sente.push(currentSenteSegment);
        }
        if (currentGoteSegment.length > 0) {
            gote.push(currentGoteSegment);
        }

        return { senteSegments: sente, goteSegments: gote };
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
        // コンパクト表示用のSVGコンテンツ
        const compactSvg = (
            <svg
                width="100%"
                height={height}
                className="block"
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

                {/* 評価値ライン（先手：朱色） */}
                {senteSegments.map((segment) => (
                    <polyline
                        key={`sente-${segment[0]}`}
                        points={segment.join(" ")}
                        fill="none"
                        stroke="hsl(var(--wafuu-shu, 10 75% 50%))"
                        strokeWidth="2"
                        vectorEffect="non-scaling-stroke"
                    />
                ))}

                {/* 評価値ライン（後手：藍色） */}
                {goteSegments.map((segment) => (
                    <polyline
                        key={`gote-${segment[0]}`}
                        points={segment.join(" ")}
                        fill="none"
                        stroke="hsl(var(--wafuu-ai, 210 55% 45%))"
                        strokeWidth="2"
                        vectorEffect="non-scaling-stroke"
                    />
                ))}

                {/* 現在位置マーカー（縦線） */}
                {currentMarker && (
                    <line
                        x1={currentMarker.x}
                        y1={padding.top}
                        x2={currentMarker.x}
                        y2={padding.top + graphHeight}
                        stroke="hsl(var(--primary, 210 100% 50%))"
                        strokeWidth="2"
                        vectorEffect="non-scaling-stroke"
                    />
                )}
            </svg>
        );

        // コンパクト表示の内部コンテンツ
        const compactContent = (
            <>
                {/* Y軸ラベル */}
                {yAxisLabels
                    .filter((_, i) => i % 2 === 0) // 上・中・下の3つだけ表示
                    .map((label) => (
                        <span
                            key={label.value}
                            className="absolute left-0 text-[10px] text-muted-foreground -translate-y-1/2 w-7 text-right pr-1"
                            style={{ top: label.position }}
                        >
                            {label.value}
                        </span>
                    ))}

                {/* グラフ本体 */}
                {/* onClickがある場合は拡大を優先（手数選択は拡大後のモーダルで） */}
                {/* onPlySelectのみの場合は手数選択 */}
                {onClick ? (
                    <button
                        ref={(el) => {
                            (
                                graphContainerRef as React.MutableRefObject<HTMLElement | null>
                            ).current = el;
                        }}
                        type="button"
                        className="ml-8 bg-transparent border-0 p-0 block text-left w-[calc(100%-32px)] cursor-pointer"
                        style={{ height }}
                        onClick={onClick}
                        aria-label="評価値グラフを拡大表示"
                        title="クリックで拡大"
                    >
                        {compactSvg}
                    </button>
                ) : onPlySelect ? (
                    <button
                        ref={(el) => {
                            (
                                graphContainerRef as React.MutableRefObject<HTMLElement | null>
                            ).current = el;
                        }}
                        type="button"
                        className="ml-8 bg-transparent border-0 p-0 block text-left w-[calc(100%-32px)] cursor-crosshair"
                        style={{ height }}
                        onClick={handleGraphClick}
                        aria-label="グラフをクリックして手数を選択"
                    >
                        {compactSvg}
                    </button>
                ) : (
                    <div
                        ref={(el) => {
                            (
                                graphContainerRef as React.MutableRefObject<HTMLElement | null>
                            ).current = el;
                        }}
                        className="ml-8"
                        style={{ height }}
                    >
                        {compactSvg}
                    </div>
                )}

                {/* X軸ラベル（手数） */}
                <div className="flex justify-between ml-8 mt-0.5 text-[10px] text-muted-foreground">
                    {xAxisLabels.map((ply) => (
                        <span key={ply}>{ply}手</span>
                    ))}
                </div>
            </>
        );

        // グラフ本体にボタンがあるので、外側は常にdiv
        return <div className="relative w-full">{compactContent}</div>;
    }

    // 通常表示
    const displayMax = scaleMax / 100;

    // SVGグラフコンテンツ
    const graphSvg = (
        <svg
            width="100%"
            height={height}
            className="block"
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

            {/* 評価値ライン（先手：朱色） */}
            {senteSegments.map((segment) => (
                <polyline
                    key={`sente-${segment[0]}`}
                    points={segment.join(" ")}
                    fill="none"
                    stroke="hsl(var(--wafuu-shu))"
                    strokeWidth="2"
                    vectorEffect="non-scaling-stroke"
                />
            ))}

            {/* 評価値ライン（後手：藍色） */}
            {goteSegments.map((segment) => (
                <polyline
                    key={`gote-${segment[0]}`}
                    points={segment.join(" ")}
                    fill="none"
                    stroke="hsl(var(--wafuu-ai))"
                    strokeWidth="2"
                    vectorEffect="non-scaling-stroke"
                />
            ))}

            {/* 現在位置マーカー（縦線） */}
            {currentMarker && (
                <line
                    x1={currentMarker.x}
                    y1={padding.top}
                    x2={currentMarker.x}
                    y2={padding.top + graphHeight}
                    stroke="hsl(var(--primary))"
                    strokeWidth="2"
                    vectorEffect="non-scaling-stroke"
                />
            )}
        </svg>
    );

    // 外側のコンテナ（onClick用）
    const outerContent = (
        <>
            <div className="flex justify-between items-center mb-1.5">
                <span className="font-bold text-sm">評価値推移</span>
                {/* 拡大ボタン（onPlySelectとonClickの両方がある場合） */}
                {onPlySelect && onClick && (
                    <button
                        type="button"
                        className="p-1 text-muted-foreground hover:text-foreground hover:bg-accent/50 rounded transition-colors"
                        onClick={onClick}
                        aria-label="評価値グラフを拡大表示"
                        title="拡大表示"
                    >
                        <svg
                            width="14"
                            height="14"
                            viewBox="0 0 24 24"
                            fill="none"
                            stroke="currentColor"
                            strokeWidth="2"
                            strokeLinecap="round"
                            strokeLinejoin="round"
                            aria-hidden="true"
                        >
                            <path d="M15 3h6v6M14 10l6.1-6.1M9 21H3v-6M10 14l-6.1 6.1" />
                        </svg>
                    </button>
                )}
            </div>
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

                {/* グラフ本体（クリック可能領域） */}
                {onPlySelect ? (
                    <button
                        ref={(el) => {
                            (
                                graphContainerRef as React.MutableRefObject<HTMLElement | null>
                            ).current = el;
                        }}
                        type="button"
                        className="ml-5 bg-transparent border-0 p-0 block text-left w-[calc(100%-20px)] cursor-crosshair"
                        style={{ height }}
                        onClick={handleGraphClick}
                        aria-label="グラフをクリックして手数を選択"
                    >
                        {graphSvg}
                    </button>
                ) : (
                    <div
                        ref={(el) => {
                            (
                                graphContainerRef as React.MutableRefObject<HTMLElement | null>
                            ).current = el;
                        }}
                        className="ml-5 w-[calc(100%-20px)]"
                        style={{ height }}
                    >
                        {graphSvg}
                    </div>
                )}
            </div>

            {/* 手数表示 */}
            <div className="flex justify-between mt-1 ml-5">
                <span className="text-[10px] text-muted-foreground">0</span>
                <span className="text-[10px] text-muted-foreground">
                    {Math.max(evalHistory.length - 1, 0)}手
                </span>
            </div>
        </>
    );

    // onPlySelectがある場合は内部にボタンがあるため、外側はdivにする
    // onPlySelectがなくonClickのみの場合は外側をボタンにする
    if (onPlySelect) {
        // 内部にボタンがあるので外側はdiv
        return (
            <div className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)]">
                {outerContent}
            </div>
        );
    }

    if (onClick) {
        return (
            <button
                type="button"
                className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)] text-left cursor-pointer"
                onClick={onClick}
                aria-label="評価値グラフを拡大表示"
            >
                {outerContent}
            </button>
        );
    }

    return (
        <div className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)]">
            {outerContent}
        </div>
    );
}
