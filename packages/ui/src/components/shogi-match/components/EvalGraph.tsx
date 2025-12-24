/**
 * 評価値グラフコンポーネント
 *
 * 横軸に手数、縦軸に評価値を表示する折れ線グラフ
 */

import type { CSSProperties, ReactElement } from "react";
import { useMemo } from "react";
import type { EvalHistory } from "../utils/kifFormat";
import { evalToY } from "../utils/kifFormat";

const baseCard: CSSProperties = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
    width: "var(--panel-width)",
};

const headerStyle: CSSProperties = {
    fontWeight: 700,
    marginBottom: "6px",
    fontSize: "14px",
};

const graphContainerStyle: CSSProperties = {
    position: "relative",
    width: "100%",
};

const labelStyle: CSSProperties = {
    position: "absolute",
    fontSize: "10px",
    color: "hsl(var(--muted-foreground, 0 0% 48%))",
};

interface EvalGraphProps {
    /** 評価値の履歴 */
    evalHistory: EvalHistory[];
    /** 現在の手数（マーカー表示用） */
    currentPly: number;
    /** コンパクト表示（スマホ用） */
    compact?: boolean;
    /** グラフの高さ（px） */
    height?: number;
    /** 評価値のクランプ範囲（センチポーン） */
    clampValue?: number;
}

/**
 * 評価値グラフ
 */
export function EvalGraph({
    evalHistory,
    currentPly,
    compact = false,
    height: customHeight,
    clampValue = 2000,
}: EvalGraphProps): ReactElement {
    const height = customHeight ?? (compact ? 60 : 80);
    const padding = { top: 4, bottom: 4, left: 0, right: 0 };
    const graphHeight = height - padding.top - padding.bottom;

    // ポイントの計算
    const points = useMemo(() => {
        if (evalHistory.length === 0) return "";

        const maxPly = Math.max(evalHistory.length - 1, 1);

        return evalHistory
            .map((entry, index) => {
                const x = (index / maxPly) * 100;
                const y =
                    padding.top + evalToY(entry.evalCp, entry.evalMate, graphHeight, clampValue);
                return `${x}%,${y}`;
            })
            .join(" ");
    }, [evalHistory, graphHeight, clampValue, padding.top]);

    // 現在位置のマーカー
    const currentMarker = useMemo(() => {
        if (currentPly < 0 || currentPly >= evalHistory.length) return null;

        const maxPly = Math.max(evalHistory.length - 1, 1);
        const entry = evalHistory[currentPly];
        const x = (currentPly / maxPly) * 100;
        const y = padding.top + evalToY(entry?.evalCp, entry?.evalMate, graphHeight, clampValue);

        return { x: `${x}%`, y };
    }, [currentPly, evalHistory, graphHeight, clampValue, padding.top]);

    // 塗りつぶし領域のパス
    const fillPath = useMemo(() => {
        if (evalHistory.length === 0) return "";

        const maxPly = Math.max(evalHistory.length - 1, 1);
        const centerY = padding.top + graphHeight / 2;

        const pathPoints = evalHistory.map((entry, index) => {
            const x = (index / maxPly) * 100;
            const y = padding.top + evalToY(entry.evalCp, entry.evalMate, graphHeight, clampValue);
            return { x, y };
        });

        // 上半分（先手有利）のパス
        const upperPath = pathPoints
            .map((p, i) => {
                const y = Math.min(p.y, centerY);
                return `${i === 0 ? "M" : "L"} ${p.x}% ${y}`;
            })
            .join(" ");

        const upperClose = `L ${100}% ${centerY} L 0% ${centerY} Z`;

        return upperPath + upperClose;
    }, [evalHistory, graphHeight, clampValue, padding.top]);

    if (compact) {
        // コンパクト表示（ヘッダーなし）
        return (
            <div style={{ ...graphContainerStyle, height }}>
                <svg
                    width="100%"
                    height={height}
                    style={{ display: "block" }}
                    viewBox={`0 0 100 ${height}`}
                    preserveAspectRatio="none"
                    role="img"
                    aria-label="評価値推移グラフ"
                >
                    {/* 中央線 */}
                    <line
                        x1="0%"
                        y1={padding.top + graphHeight / 2}
                        x2="100%"
                        y2={padding.top + graphHeight / 2}
                        stroke="hsl(var(--border, 0 0% 86%))"
                        strokeWidth="1"
                        vectorEffect="non-scaling-stroke"
                    />

                    {/* 塗りつぶし領域（先手有利部分） */}
                    {fillPath && (
                        <path
                            d={fillPath}
                            fill="hsla(var(--wafuu-shu, 350 80% 45%), 0.15)"
                            stroke="none"
                        />
                    )}

                    {/* 評価値ライン */}
                    {points && (
                        <polyline
                            points={points}
                            fill="none"
                            stroke="hsl(var(--wafuu-shu, 350 80% 45%))"
                            strokeWidth="2"
                            vectorEffect="non-scaling-stroke"
                        />
                    )}

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
        );
    }

    // 通常表示
    return (
        <div style={baseCard}>
            <div style={headerStyle}>評価値推移</div>
            <div style={{ ...graphContainerStyle, height }}>
                {/* 左側ラベル */}
                <span
                    style={{
                        ...labelStyle,
                        top: padding.top,
                        left: 0,
                        transform: "translateY(-50%)",
                    }}
                >
                    +{clampValue / 100}
                </span>
                <span
                    style={{
                        ...labelStyle,
                        top: padding.top + graphHeight / 2,
                        left: 0,
                        transform: "translateY(-50%)",
                    }}
                >
                    0
                </span>
                <span
                    style={{
                        ...labelStyle,
                        bottom: padding.bottom,
                        left: 0,
                        transform: "translateY(50%)",
                    }}
                >
                    -{clampValue / 100}
                </span>

                <svg
                    width="100%"
                    height={height}
                    style={{ display: "block", marginLeft: "20px", width: "calc(100% - 20px)" }}
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
                        stroke="hsl(var(--border, 0 0% 86%))"
                        strokeWidth="0.5"
                        vectorEffect="non-scaling-stroke"
                        strokeDasharray="2,2"
                    />
                    <line
                        x1="0%"
                        y1={padding.top + graphHeight}
                        x2="100%"
                        y2={padding.top + graphHeight}
                        stroke="hsl(var(--border, 0 0% 86%))"
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
                        stroke="hsl(var(--border, 0 0% 70%))"
                        strokeWidth="1"
                        vectorEffect="non-scaling-stroke"
                    />

                    {/* 塗りつぶし領域（先手有利部分） */}
                    {fillPath && (
                        <path
                            d={fillPath}
                            fill="hsla(var(--wafuu-shu, 350 80% 45%), 0.15)"
                            stroke="none"
                        />
                    )}

                    {/* 評価値ライン */}
                    {points && (
                        <polyline
                            points={points}
                            fill="none"
                            stroke="hsl(var(--wafuu-shu, 350 80% 45%))"
                            strokeWidth="2"
                            vectorEffect="non-scaling-stroke"
                        />
                    )}

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

            {/* 手数表示 */}
            <div
                style={{
                    display: "flex",
                    justifyContent: "space-between",
                    marginTop: "4px",
                    marginLeft: "20px",
                }}
            >
                <span style={{ ...labelStyle, position: "static" }}>0</span>
                <span style={{ ...labelStyle, position: "static" }}>
                    {Math.max(evalHistory.length - 1, 0)}手
                </span>
            </div>
        </div>
    );
}
