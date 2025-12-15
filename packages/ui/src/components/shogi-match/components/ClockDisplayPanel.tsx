import type { Player } from "@shogi/app-core";
import type { ReactElement } from "react";
import type { TickState } from "../hooks/useClockManager";
import { formatTime } from "../utils/timeFormat";

const baseCard = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
};

export interface ClockDisplayPanelProps {
    /** 時計の状態 */
    clocks: TickState;
}

export function ClockDisplayPanel({ clocks }: ClockDisplayPanelProps): ReactElement {
    const renderClock = (side: Player) => {
        const clock = clocks[side];
        const ticking = clocks.ticking === side;
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
                    {side === "sente" ? "先手" : "後手"}
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
            <div style={{ fontWeight: 700, marginBottom: "6px" }}>時計</div>
            <div style={{ display: "flex", flexDirection: "column", gap: "6px" }}>
                {renderClock("sente")}
                {renderClock("gote")}
            </div>
        </div>
    );
}
