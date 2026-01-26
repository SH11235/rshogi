import type {
    EngineClient,
    EngineEvent,
    SearchHandle,
    SearchLimits,
    ThreadInfo,
} from "@shogi/engine-client";
import type { CSSProperties, ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button } from "./button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogHeader,
    DialogTitle,
    DialogTrigger,
} from "./dialog";
import { Input } from "./input";

type PanelStatus = "idle" | "init" | "ready" | "searching" | "stopping" | "error";

type EngineLogEntry = {
    id: number;
    text: string;
    event: EngineEvent;
    timestamp: Date;
};

type LimitsFormState = {
    depth: string;
    nodes: string;
    byoyomi: string;
    movetime: string;
    ponder: boolean;
};

type UsiOptionType = "spin" | "check";

type UsiOptionDefinition = {
    name: string;
    type: UsiOptionType;
    defaultValue: number | boolean;
    min?: number;
    max?: number;
    note?: string;
};

type EnginePosition = {
    label?: string;
    sfen: string;
    moves?: string[];
};

interface EngineControlPanelProps {
    engine: EngineClient;
    position?: EnginePosition;
    triggerLabel?: string;
    maxLogs?: number;
}

const DEFAULT_POSITION: EnginePosition = { label: "開始局面 (startpos)", sfen: "startpos" };
const DEFAULT_LIMITS: LimitsFormState = {
    depth: "",
    nodes: "",
    byoyomi: "5000",
    movetime: "",
    ponder: false,
};
const DEFAULT_MAX_LOGS = 60;

const LIMIT_INPUT_IDS = {
    depth: "engine-limit-depth",
    byoyomi: "engine-limit-byoyomi",
    nodes: "engine-limit-nodes",
    movetime: "engine-limit-movetime",
    ponder: "engine-limit-ponder",
} as const;

const USI_OPTIONS: UsiOptionDefinition[] = [
    {
        name: "Threads",
        type: "spin",
        defaultValue: 1,
        min: 1,
        max: 4,
        note: "並列探索スレッド数 (次回init時に適用)",
    },
    { name: "USI_Hash", type: "spin", defaultValue: 256, min: 1, max: 4096 },
    { name: "USI_Ponder", type: "check", defaultValue: false },
    { name: "Stochastic_Ponder", type: "check", defaultValue: false },
    { name: "MultiPV", type: "spin", defaultValue: 1, min: 1, max: 500 },
    { name: "NetworkDelay", type: "spin", defaultValue: 120, min: 0, max: 10000 },
    { name: "NetworkDelay2", type: "spin", defaultValue: 1120, min: 0, max: 10000 },
    { name: "MinimumThinkingTime", type: "spin", defaultValue: 2000, min: 1000, max: 100000 },
    { name: "SlowMover", type: "spin", defaultValue: 100, min: 1, max: 1000 },
    { name: "MaxMovesToDraw", type: "spin", defaultValue: 100000, min: 0, max: 100000 },
    { name: "Skill Level", type: "spin", defaultValue: 20, min: 0, max: 20 },
    { name: "UCI_LimitStrength", type: "check", defaultValue: false },
    { name: "UCI_Elo", type: "spin", defaultValue: 0, min: 0, max: 4000 },
];

const surfaceStyle: CSSProperties = {
    background: "hsl(var(--card, 0 0% 100%))",
    color: "hsl(var(--foreground, 222 47% 11%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "16px",
    boxShadow: "0 18px 38px rgba(0, 0, 0, 0.18)",
};

const gridStyle: CSSProperties = {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))",
    gap: "12px",
};

const labelStyle: CSSProperties = {
    fontSize: "12px",
    color: "hsl(var(--muted-foreground, 0 0% 45%))",
};
const inputStyle: CSSProperties = { background: "hsl(var(--background, 0 0% 100%))" };

function formatEvent(event: EngineEvent): string {
    if (event.type === "bestmove") {
        return event.ponder
            ? `bestmove ${event.move} (ponder ${event.ponder})`
            : `bestmove ${event.move}`;
    }
    if (event.type === "info") {
        const score =
            event.scoreMate !== undefined
                ? `mate ${event.scoreMate}`
                : event.scoreCp !== undefined
                  ? `cp ${event.scoreCp}`
                  : "";
        const pv = event.pv && event.pv.length > 0 ? ` pv ${event.pv.join(" ")}` : "";
        return [
            `info depth ${event.depth ?? "-"}`,
            event.nodes !== undefined ? `nodes ${event.nodes}` : null,
            event.nps !== undefined ? `nps ${event.nps}` : null,
            score ? `score ${score}` : null,
            pv ? pv : null,
        ]
            .filter(Boolean)
            .join(" ");
    }
    return `error ${event.message}`;
}

function parseNumber(value: string): number | undefined {
    const num = Number.parseInt(value, 10);
    return Number.isFinite(num) ? num : undefined;
}

function buildLimits(state: LimitsFormState): SearchLimits {
    const limits: SearchLimits = {};
    const depth = parseNumber(state.depth);
    if (depth !== undefined) limits.maxDepth = depth;

    const nodes = parseNumber(state.nodes);
    if (nodes !== undefined) limits.nodes = nodes;

    const byoyomi = parseNumber(state.byoyomi);
    if (byoyomi !== undefined) limits.byoyomiMs = byoyomi;

    const movetime = parseNumber(state.movetime);
    if (movetime !== undefined) limits.movetimeMs = movetime;

    return limits;
}

function nextLogId(): number {
    return Date.now() + Math.random();
}

function normalizeOptionId(name: string): string {
    return `usi-option-${name.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`;
}

export function EngineControlPanel({
    engine,
    position = DEFAULT_POSITION,
    triggerLabel = "エンジン操作パネル",
    maxLogs = DEFAULT_MAX_LOGS,
}: EngineControlPanelProps): ReactElement {
    const [open, setOpen] = useState(false);
    const [status, setStatus] = useState<PanelStatus>("idle");
    const [limits, setLimits] = useState<LimitsFormState>(DEFAULT_LIMITS);
    const [logs, setLogs] = useState<EngineLogEntry[]>([]);
    const [bestmove, setBestmove] = useState<string | null>(null);
    const [initialized, setInitialized] = useState(false);
    const [busy, setBusy] = useState(false);
    const [customOption, setCustomOption] = useState({ name: "", value: "" });
    const [threadInfo, setThreadInfo] = useState<ThreadInfo | null>(null);
    const [latestNps, setLatestNps] = useState<number | null>(null);
    const handleRef = useRef<SearchHandle | null>(null);

    const updateThreadInfo = useCallback(() => {
        if (engine.getThreadInfo) {
            setThreadInfo(engine.getThreadInfo());
        }
    }, [engine]);

    const optionDefaults = useMemo(() => {
        const defaults: Record<string, string> = {};
        for (const opt of USI_OPTIONS) {
            defaults[opt.name] = String(opt.defaultValue);
        }
        return defaults;
    }, []);
    const [optionValues, setOptionValues] = useState<Record<string, string>>(optionDefaults);

    useEffect(() => {
        setOptionValues(optionDefaults);
    }, [optionDefaults]);

    useEffect(() => {
        // Update thread info on mount and when engine changes
        updateThreadInfo();
    }, [updateThreadInfo]);

    useEffect(() => {
        const unsubscribe = engine.subscribe((event) => {
            setLogs((prev) => {
                const entry: EngineLogEntry = {
                    id: nextLogId(),
                    text: formatEvent(event),
                    event,
                    timestamp: new Date(),
                };
                const next = [entry, ...prev];
                if (next.length > maxLogs) {
                    return next.slice(0, maxLogs);
                }
                return next;
            });
            if (event.type === "info" && event.nps !== undefined) {
                setLatestNps(event.nps);
            }
            if (event.type === "bestmove") {
                setBestmove(event.move);
                setStatus("idle");
                handleRef.current = null;
            }
            if (event.type === "error") {
                setStatus("error");
            }
        });

        return () => {
            const handle = handleRef.current;
            if (handle) {
                handle.cancel().catch(() => undefined);
                handleRef.current = null;
            }
            unsubscribe();
        };
    }, [engine, maxLogs]);

    const pushUiError = (message: string) => {
        setLogs((prev) => {
            const entry: EngineLogEntry = {
                id: nextLogId(),
                text: `ui error: ${message}`,
                event: { type: "error", message },
                timestamp: new Date(),
            };
            const next = [entry, ...prev];
            return next.length > maxLogs ? next.slice(0, maxLogs) : next;
        });
        setStatus("error");
    };

    const ensureInitialized = async () => {
        if (initialized) return;
        setStatus("init");
        await engine.init();
        await engine.loadPosition(position.sfen, position.moves);
        setInitialized(true);
        setStatus("ready");
        // Update thread info after init
        updateThreadInfo();
    };

    const applyOptions = async (): Promise<boolean> => {
        let hadError = false;
        for (const opt of USI_OPTIONS) {
            const raw = optionValues[opt.name];
            if (raw === undefined || raw === "") continue;
            if (opt.type === "check") {
                const boolValue = raw === "true" || raw === "1" || raw === "on" || raw === "yes";
                await engine.setOption(opt.name, boolValue);
                continue;
            }
            const numValue = parseNumber(raw);
            if (numValue === undefined) {
                pushUiError(`${opt.name} に数値を入力してください`);
                hadError = true;
                continue;
            }
            await engine.setOption(opt.name, numValue);
        }

        if (customOption.name.trim() && customOption.value.trim()) {
            const valueText = customOption.value.trim();
            const numValue = parseNumber(valueText);
            const normalizedValue =
                valueText === "true" || valueText === "false"
                    ? valueText === "true"
                    : numValue !== undefined
                      ? numValue
                      : valueText;
            await engine.setOption(customOption.name.trim(), normalizedValue);
        }

        return !hadError;
    };

    const handleInitClick = async () => {
        if (busy) return;
        setBusy(true);
        try {
            await ensureInitialized();
        } catch (error) {
            pushUiError(String(error));
        } finally {
            setBusy(false);
        }
    };

    const handleStart = async () => {
        if (busy || status === "searching") return;
        setBusy(true);
        try {
            await ensureInitialized();
            const ok = await applyOptions();
            if (!ok) {
                return;
            }
            const searchLimits = buildLimits(limits);
            setStatus("searching");
            const handle = await engine.search({ limits: searchLimits, ponder: limits.ponder });
            handleRef.current = handle;
        } catch (error) {
            pushUiError(String(error));
        } finally {
            setBusy(false);
        }
    };

    const handleStop = async () => {
        if (busy) return;
        setBusy(true);
        setStatus("stopping");
        try {
            const handle = handleRef.current;
            if (handle) {
                await handle.cancel().catch(() => undefined);
                handleRef.current = null;
            }
            await engine.stop();
            setStatus("idle");
        } catch (error) {
            pushUiError(String(error));
        } finally {
            setBusy(false);
        }
    };

    const resetLogs = () => setLogs([]);

    const statusLabel =
        status === "idle"
            ? "待機中"
            : status === "init"
              ? "初期化中..."
              : status === "ready"
                ? "準備完了"
                : status === "searching"
                  ? "探索中..."
                  : status === "stopping"
                    ? "停止処理中..."
                    : "エラー";

    return (
        <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
            <div
                style={{
                    ...surfaceStyle,
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: "12px",
                }}
            >
                <div>
                    <div style={{ fontWeight: 600, marginBottom: 4 }}>エンジン操作</div>
                    <div
                        style={{
                            fontSize: "13px",
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        }}
                    >
                        状態: {statusLabel} {bestmove ? `| 最終 bestmove: ${bestmove}` : ""}
                    </div>
                </div>
                <Dialog open={open} onOpenChange={setOpen}>
                    <DialogTrigger asChild>
                        <Button
                            type="button"
                            style={{
                                paddingInline: "14px",
                                height: "40px",
                                borderRadius: "8px",
                                background:
                                    "linear-gradient(120deg, hsl(var(--primary, 15 86% 55%)), hsl(var(--accent, 37 94% 50%)))",
                                color: "hsl(var(--primary-foreground, 0 0% 100%))",
                                border: "none",
                                cursor: "pointer",
                            }}
                        >
                            {triggerLabel}
                        </Button>
                    </DialogTrigger>
                    <DialogContent
                        overlayStyle={{ backgroundColor: "rgba(8, 10, 20, 0.58)" }}
                        style={{ width: "min(1040px, calc(100% - 24px))" }}
                    >
                        <DialogHeader>
                            <DialogTitle>エンジン操作パネル</DialogTitle>
                            <DialogDescription>
                                Web / Desktop 共通の操作モーダル。USI オプションは engine-usi
                                の定義に合わせています。
                            </DialogDescription>
                        </DialogHeader>

                        <div style={{ display: "flex", flexDirection: "column", gap: "16px" }}>
                            <section style={{ ...surfaceStyle, padding: "12px" }}>
                                <div
                                    style={{
                                        display: "flex",
                                        justifyContent: "space-between",
                                        gap: "12px",
                                    }}
                                >
                                    <div>
                                        <div style={{ fontWeight: 600 }}>接続・初期化</div>
                                        <div style={{ fontSize: "13px", color: labelStyle.color }}>
                                            状態: {statusLabel}
                                        </div>
                                        <div style={{ fontSize: "12px", color: labelStyle.color }}>
                                            局面: {position.label ?? position.sfen}
                                        </div>
                                    </div>
                                    <div
                                        style={{
                                            display: "flex",
                                            gap: "8px",
                                            alignItems: "center",
                                        }}
                                    >
                                        <Button
                                            type="button"
                                            onClick={handleInitClick}
                                            disabled={busy || status === "init"}
                                            style={{ paddingInline: "12px" }}
                                        >
                                            init
                                        </Button>
                                        <Button
                                            type="button"
                                            onClick={resetLogs}
                                            disabled={logs.length === 0}
                                            variant="secondary"
                                            style={{ paddingInline: "12px" }}
                                        >
                                            ログクリア
                                        </Button>
                                    </div>
                                </div>
                            </section>

                            <section
                                style={{
                                    ...surfaceStyle,
                                    padding: "12px",
                                    background:
                                        "linear-gradient(135deg, hsl(var(--card, 0 0% 100%)), hsl(210 40% 98%))",
                                }}
                            >
                                <div style={{ fontWeight: 600, marginBottom: 8 }}>
                                    デバッグ情報 (開発者向け)
                                </div>
                                <div
                                    style={{
                                        display: "grid",
                                        gridTemplateColumns: "repeat(auto-fit, minmax(140px, 1fr))",
                                        gap: "8px",
                                        fontSize: "12px",
                                    }}
                                >
                                    <div
                                        style={{
                                            padding: "8px",
                                            background: "hsl(var(--background, 0 0% 100%))",
                                            borderRadius: "6px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    >
                                        <div style={{ color: labelStyle.color }}>
                                            アクティブスレッド
                                        </div>
                                        <div
                                            style={{
                                                fontWeight: 600,
                                                fontSize: "16px",
                                                color:
                                                    threadInfo && threadInfo.activeThreads > 1
                                                        ? "hsl(142 76% 36%)"
                                                        : "inherit",
                                            }}
                                        >
                                            {threadInfo?.activeThreads ?? "-"}
                                        </div>
                                    </div>
                                    <div
                                        style={{
                                            padding: "8px",
                                            background: "hsl(var(--background, 0 0% 100%))",
                                            borderRadius: "6px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    >
                                        <div style={{ color: labelStyle.color }}>最大スレッド</div>
                                        <div style={{ fontWeight: 600, fontSize: "16px" }}>
                                            {threadInfo?.maxThreads ?? "-"}
                                        </div>
                                    </div>
                                    <div
                                        style={{
                                            padding: "8px",
                                            background: "hsl(var(--background, 0 0% 100%))",
                                            borderRadius: "6px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    >
                                        <div style={{ color: labelStyle.color }}>
                                            ハードウェア並列数
                                        </div>
                                        <div style={{ fontWeight: 600, fontSize: "16px" }}>
                                            {threadInfo?.hardwareConcurrency ?? "-"}
                                        </div>
                                    </div>
                                    <div
                                        style={{
                                            padding: "8px",
                                            background: "hsl(var(--background, 0 0% 100%))",
                                            borderRadius: "6px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    >
                                        <div style={{ color: labelStyle.color }}>
                                            スレッド利用可能
                                        </div>
                                        <div
                                            style={{
                                                fontWeight: 600,
                                                fontSize: "14px",
                                                color: threadInfo?.threadedAvailable
                                                    ? "hsl(142 76% 36%)"
                                                    : "hsl(0 72% 51%)",
                                            }}
                                        >
                                            {threadInfo?.threadedAvailable ? "Yes" : "No"}
                                        </div>
                                    </div>
                                    <div
                                        style={{
                                            padding: "8px",
                                            background: "hsl(var(--background, 0 0% 100%))",
                                            borderRadius: "6px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    >
                                        <div style={{ color: labelStyle.color }}>最新 NPS</div>
                                        <div style={{ fontWeight: 600, fontSize: "16px" }}>
                                            {latestNps !== null ? latestNps.toLocaleString() : "-"}
                                        </div>
                                    </div>
                                    <div
                                        style={{
                                            padding: "8px",
                                            background: "hsl(var(--background, 0 0% 100%))",
                                            borderRadius: "6px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    >
                                        <div style={{ color: labelStyle.color }}>
                                            crossOriginIsolated
                                        </div>
                                        <div
                                            style={{
                                                fontWeight: 600,
                                                fontSize: "14px",
                                                color:
                                                    typeof crossOriginIsolated !== "undefined" &&
                                                    crossOriginIsolated
                                                        ? "hsl(142 76% 36%)"
                                                        : "hsl(0 72% 51%)",
                                            }}
                                        >
                                            {typeof crossOriginIsolated !== "undefined"
                                                ? crossOriginIsolated
                                                    ? "true"
                                                    : "false"
                                                : "N/A"}
                                        </div>
                                    </div>
                                </div>
                                <div
                                    style={{
                                        marginTop: "8px",
                                        fontSize: "11px",
                                        color: labelStyle.color,
                                    }}
                                >
                                    スレッド利用には crossOriginIsolated=true と SharedArrayBuffer
                                    が必要です
                                </div>
                            </section>

                            <section style={surfaceStyle}>
                                <div style={{ fontWeight: 600, marginBottom: 8 }}>
                                    探索パラメータ
                                </div>
                                <div style={gridStyle}>
                                    <label
                                        htmlFor={LIMIT_INPUT_IDS.depth}
                                        style={{
                                            display: "flex",
                                            flexDirection: "column",
                                            gap: "4px",
                                        }}
                                    >
                                        <span style={labelStyle}>depth</span>
                                        <Input
                                            id={LIMIT_INPUT_IDS.depth}
                                            type="number"
                                            min={1}
                                            value={limits.depth}
                                            onChange={(e) =>
                                                setLimits({ ...limits, depth: e.target.value })
                                            }
                                            placeholder="例: 12"
                                            style={inputStyle}
                                        />
                                    </label>
                                    <label
                                        htmlFor={LIMIT_INPUT_IDS.byoyomi}
                                        style={{
                                            display: "flex",
                                            flexDirection: "column",
                                            gap: "4px",
                                        }}
                                    >
                                        <span style={labelStyle}>byoyomi (ms)</span>
                                        <Input
                                            id={LIMIT_INPUT_IDS.byoyomi}
                                            type="number"
                                            min={0}
                                            value={limits.byoyomi}
                                            onChange={(e) =>
                                                setLimits({ ...limits, byoyomi: e.target.value })
                                            }
                                            placeholder="例: 5000"
                                            style={inputStyle}
                                        />
                                    </label>
                                    <label
                                        htmlFor={LIMIT_INPUT_IDS.nodes}
                                        style={{
                                            display: "flex",
                                            flexDirection: "column",
                                            gap: "4px",
                                        }}
                                    >
                                        <span style={labelStyle}>nodes</span>
                                        <Input
                                            id={LIMIT_INPUT_IDS.nodes}
                                            type="number"
                                            min={0}
                                            value={limits.nodes}
                                            onChange={(e) =>
                                                setLimits({ ...limits, nodes: e.target.value })
                                            }
                                            placeholder="例: 100000"
                                            style={inputStyle}
                                        />
                                    </label>
                                    <label
                                        htmlFor={LIMIT_INPUT_IDS.movetime}
                                        style={{
                                            display: "flex",
                                            flexDirection: "column",
                                            gap: "4px",
                                        }}
                                    >
                                        <span style={labelStyle}>movetime (ms)</span>
                                        <Input
                                            id={LIMIT_INPUT_IDS.movetime}
                                            type="number"
                                            min={0}
                                            value={limits.movetime}
                                            onChange={(e) =>
                                                setLimits({ ...limits, movetime: e.target.value })
                                            }
                                            placeholder="例: 1000"
                                            style={inputStyle}
                                        />
                                    </label>
                                </div>
                                <label
                                    htmlFor={LIMIT_INPUT_IDS.ponder}
                                    style={{
                                        display: "flex",
                                        alignItems: "center",
                                        gap: "8px",
                                        marginTop: "10px",
                                    }}
                                >
                                    <input
                                        id={LIMIT_INPUT_IDS.ponder}
                                        type="checkbox"
                                        checked={limits.ponder}
                                        onChange={(e) =>
                                            setLimits({ ...limits, ponder: e.target.checked })
                                        }
                                    />
                                    <span style={{ fontSize: "13px" }}>ponder を有効化</span>
                                </label>
                            </section>

                            <section style={surfaceStyle}>
                                <div style={{ fontWeight: 600, marginBottom: 8 }}>
                                    USI オプション
                                </div>
                                <div style={gridStyle}>
                                    {USI_OPTIONS.map((opt) => (
                                        <div
                                            key={opt.name}
                                            style={{
                                                display: "flex",
                                                flexDirection: "column",
                                                gap: "4px",
                                            }}
                                        >
                                            <label
                                                htmlFor={normalizeOptionId(opt.name)}
                                                style={{ fontSize: "13px", fontWeight: 600 }}
                                            >
                                                {opt.name}
                                            </label>
                                            <span style={labelStyle}>
                                                default {String(opt.defaultValue)}
                                                {opt.min !== undefined ? ` | min ${opt.min}` : ""}{" "}
                                                {opt.max !== undefined ? `| max ${opt.max}` : ""}
                                            </span>
                                            {opt.type === "check" ? (
                                                <div
                                                    style={{
                                                        display: "flex",
                                                        alignItems: "center",
                                                        gap: "8px",
                                                    }}
                                                >
                                                    <input
                                                        id={normalizeOptionId(opt.name)}
                                                        type="checkbox"
                                                        checked={optionValues[opt.name] === "true"}
                                                        onChange={(e) =>
                                                            setOptionValues({
                                                                ...optionValues,
                                                                [opt.name]: e.target.checked
                                                                    ? "true"
                                                                    : "false",
                                                            })
                                                        }
                                                    />
                                                    <span style={{ fontSize: "13px" }}>
                                                        ON / OFF
                                                    </span>
                                                </div>
                                            ) : (
                                                <Input
                                                    id={normalizeOptionId(opt.name)}
                                                    type="number"
                                                    value={optionValues[opt.name] ?? ""}
                                                    min={opt.min}
                                                    max={opt.max}
                                                    onChange={(e) =>
                                                        setOptionValues({
                                                            ...optionValues,
                                                            [opt.name]: e.target.value,
                                                        })
                                                    }
                                                    style={inputStyle}
                                                />
                                            )}
                                            {opt.note ? (
                                                <span style={labelStyle}>{opt.note}</span>
                                            ) : null}
                                        </div>
                                    ))}
                                </div>
                                <div style={{ marginTop: "12px", fontWeight: 600 }}>
                                    カスタム setoption
                                </div>
                                <div
                                    style={{
                                        display: "grid",
                                        gridTemplateColumns: "1.2fr 1fr",
                                        gap: "10px",
                                        marginTop: "6px",
                                    }}
                                >
                                    <Input
                                        placeholder="name"
                                        value={customOption.name}
                                        onChange={(e) =>
                                            setCustomOption({
                                                ...customOption,
                                                name: e.target.value,
                                            })
                                        }
                                        style={inputStyle}
                                    />
                                    <Input
                                        placeholder="value"
                                        value={customOption.value}
                                        onChange={(e) =>
                                            setCustomOption({
                                                ...customOption,
                                                value: e.target.value,
                                            })
                                        }
                                        style={inputStyle}
                                    />
                                </div>
                                <div
                                    style={{
                                        fontSize: "12px",
                                        color: labelStyle.color,
                                        marginTop: "4px",
                                    }}
                                >
                                    追加の USI オプションを送る場合に使用します（型は自動推定）。
                                </div>
                            </section>

                            <section
                                style={{
                                    ...surfaceStyle,
                                    display: "flex",
                                    gap: "10px",
                                    justifyContent: "flex-end",
                                }}
                            >
                                <Button
                                    type="button"
                                    onClick={handleStart}
                                    disabled={status === "searching" || busy}
                                    style={{ minWidth: "140px", paddingInline: "14px" }}
                                >
                                    {status === "searching" ? "探索中…" : "search / start"}
                                </Button>
                                <Button
                                    type="button"
                                    onClick={handleStop}
                                    disabled={busy || status === "idle"}
                                    variant="secondary"
                                    style={{ minWidth: "120px", paddingInline: "12px" }}
                                >
                                    stop
                                </Button>
                            </section>

                            <section
                                style={{ ...surfaceStyle, maxHeight: "280px", overflow: "auto" }}
                            >
                                <div
                                    style={{
                                        display: "flex",
                                        justifyContent: "space-between",
                                        alignItems: "center",
                                    }}
                                >
                                    <div style={{ fontWeight: 600 }}>ログ (最新が上)</div>
                                    <span style={{ fontSize: "12px", color: labelStyle.color }}>
                                        最大 {maxLogs} 件を保持
                                    </span>
                                </div>
                                <ul
                                    style={{
                                        listStyle: "none",
                                        padding: 0,
                                        marginTop: "10px",
                                        display: "flex",
                                        flexDirection: "column",
                                        gap: "6px",
                                    }}
                                >
                                    {logs.map((log) => (
                                        <li
                                            key={log.id}
                                            style={{
                                                padding: "8px 10px",
                                                background: "hsl(var(--muted, 210 40% 96.1%))",
                                                borderRadius: "8px",
                                                border: "1px solid hsl(var(--border, 0 0% 86%))",
                                            }}
                                        >
                                            <div
                                                style={{
                                                    fontSize: "11px",
                                                    color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                                    marginBottom: "2px",
                                                }}
                                            >
                                                {log.timestamp.toLocaleTimeString()}
                                            </div>
                                            <div
                                                style={{
                                                    fontFamily:
                                                        'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
                                                    fontSize: "13px",
                                                }}
                                            >
                                                {log.text}
                                            </div>
                                        </li>
                                    ))}
                                </ul>
                            </section>
                        </div>
                    </DialogContent>
                </Dialog>
            </div>
        </div>
    );
}
