import type { Player } from "@shogi/app-core";
import type { ReactElement } from "react";
import { Button } from "../../button";
import type { EngineOption, SideSetting } from "./MatchSettingsPanel";

const baseCard = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
    width: "var(--panel-width)",
};

type EngineStatus = "idle" | "thinking" | "error";

interface MatchControlsProps {
    /** 新規対局ボタンのクリックハンドラ */
    onNewGame: () => void;
    /** 停止ボタンのクリックハンドラ */
    onPause: () => void;
    /** 対局開始/再開ボタンのクリックハンドラ */
    onResume: () => void;
    /** 先手/後手の設定 */
    sides: { sente: SideSetting; gote: SideSetting };
    /** エンジンの準備状態 */
    engineReady: Record<Player, boolean>;
    /** エンジンのステータス */
    engineStatus: Record<Player, EngineStatus>;
    /** 対局実行中かどうか */
    isMatchRunning: boolean;
    /** メッセージ */
    message: string | null;
    /** エンジンオプション取得関数 */
    getEngineForSide: (side: Player) => EngineOption | undefined;
}

export function MatchControls({
    onNewGame,
    onPause,
    onResume,
    sides,
    engineReady,
    engineStatus,
    isMatchRunning,
    message,
    getEngineForSide,
}: MatchControlsProps): ReactElement {
    return (
        <div
            style={{
                ...baseCard,
                display: "flex",
                flexDirection: "column",
                gap: "10px",
            }}
        >
            <div
                style={{
                    display: "flex",
                    gap: "8px",
                    flexWrap: "wrap",
                    alignItems: "center",
                }}
            >
                <Button type="button" onClick={onNewGame} style={{ paddingInline: "12px" }}>
                    新規対局（初期化）
                </Button>
                <Button
                    type="button"
                    onClick={onPause}
                    variant="outline"
                    style={{ paddingInline: "12px" }}
                >
                    停止（自動進行オフ）
                </Button>
                <Button
                    type="button"
                    onClick={onResume}
                    variant="secondary"
                    style={{ paddingInline: "12px" }}
                >
                    対局開始 / 再開
                </Button>
            </div>
            <div
                style={{
                    fontSize: "12px",
                    color: "hsl(var(--muted-foreground, 0 0% 48%))",
                }}
            >
                状態:
                {(["sente", "gote"] as Player[]).map((side) => {
                    const sideLabel = side === "sente" ? "先手" : "後手";
                    const roleLabel = sides[side].role === "engine" ? "エンジン" : "人間";
                    if (sides[side].role !== "engine") {
                        return (
                            <span key={side}>
                                {" "}
                                [{sideLabel}: {roleLabel}]
                            </span>
                        );
                    }
                    const engineLabel = getEngineForSide(side)?.label ?? "未選択";
                    const ready = engineReady[side] ? "init済" : "未init";
                    const status = engineStatus[side];
                    return (
                        <span key={side}>
                            {" "}
                            [{sideLabel}: {roleLabel} {engineLabel} {status}/{ready}]
                        </span>
                    );
                })}
                {` | 対局: ${isMatchRunning ? "実行中" : "停止中"}`}
            </div>
            {message ? (
                <div
                    style={{
                        color: "hsl(var(--destructive, 0 72% 51%))",
                        fontSize: "13px",
                    }}
                >
                    {message}
                </div>
            ) : null}
        </div>
    );
}
