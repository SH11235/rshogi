import type { Player } from "@shogi/app-core";
import type { ReactElement } from "react";
import type { TickState } from "../hooks/useClockManager";
import { formatTime } from "../utils/timeFormat";
import type { SideSetting } from "./MatchSettingsPanel";

const baseCard = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
    width: "var(--panel-width)",
};

const ROLE_LABELS: Record<SideSetting["role"], string> = {
    human: "人",
    engine: "AI",
};

interface ClockDisplayPanelProps {
    /** 時計の状態 */
    clocks: TickState;
    /** 先手・後手の設定 */
    sides: { sente: SideSetting; gote: SideSetting };
}

export function ClockDisplayPanel({ clocks, sides }: ClockDisplayPanelProps): ReactElement {
    const getRoleLabel = (side: Player): string => ROLE_LABELS[sides[side].role];

    const renderClock = (side: Player) => {
        const clock = clocks[side];
        const ticking = clocks.ticking === side;
        const sideLabel = side === "sente" ? "☗先手" : "☖後手";
        const roleLabel = getRoleLabel(side);
        return (
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                <span
                    style={{
                        fontWeight: 700,
                        color:
                            side === "sente"
                                ? "hsl(var(--primary, 15 86% 55%))"
                                : "hsl(var(--accent, 37 94% 50%))",
                    }}
                >
                    {sideLabel}
                </span>
                <span
                    style={{
                        fontSize: "12px",
                        padding: "2px 6px",
                        borderRadius: "4px",
                        background: "hsl(var(--muted, 210 40% 96%))",
                        color: "hsl(var(--muted-foreground, 0 0% 48%))",
                    }}
                >
                    {roleLabel}
                </span>
                <span style={{ fontVariantNumeric: "tabular-nums", fontSize: "16px" }}>
                    {formatTime(clock.mainMs)} + {formatTime(clock.byoyomiMs)}
                </span>
                {ticking ? (
                    <span
                        style={{
                            display: "inline-block",
                            width: "10px",
                            height: "10px",
                            borderRadius: "50%",
                            background: "hsl(var(--primary, 15 86% 55%))",
                        }}
                    />
                ) : null}
            </div>
        );
    };

    return (
        <div style={baseCard}>
            <div
                style={{
                    display: "flex",
                    alignItems: "baseline",
                    gap: "8px",
                    marginBottom: "6px",
                }}
            >
                <span style={{ fontWeight: 700 }}>持時間</span>
                <span
                    style={{
                        fontSize: "11px",
                        color: "hsl(var(--muted-foreground, 0 0% 48%))",
                    }}
                >
                    持ち時間 + 秒読み
                </span>
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: "6px" }}>
                {renderClock("sente")}
                {renderClock("gote")}
            </div>
        </div>
    );
}
