import {
    applyMoveWithState,
    type BoardState,
    boardToMatrix,
    buildPositionString,
    cloneBoard,
    createEmptyHands,
    getAllSquares,
    getPositionService,
    type LastMove,
    movesToCsa,
    type PieceType,
    type Player,
    type PositionState,
    parseCsaMoves,
    parseMove,
    type Square,
} from "@shogi/app-core";
import type { EngineClient, EngineEvent, SearchHandle } from "@shogi/engine-client";
import type { CSSProperties, ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button } from "./button";
import { Input } from "./input";
import type { ShogiBoardCell } from "./shogi-board";
import { ShogiBoard } from "./shogi-board";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "./tooltip";

type Selection = { kind: "square"; square: string } | { kind: "hand"; piece: PieceType };
type SideRole = "human" | "engine";
type EngineStatus = "idle" | "thinking" | "error";

export type EngineOption = {
    id: string;
    label: string;
    client: EngineClient;
    kind?: "internal" | "external";
};

type SideSetting = {
    role: SideRole;
    engineId?: string;
};

type ClockSettings = Record<Player, { mainMs: number; byoyomiMs: number }>;

interface ClockState {
    mainMs: number;
    byoyomiMs: number;
}

interface TickState {
    sente: ClockState;
    gote: ClockState;
    ticking: Player | null;
    lastUpdatedAt: number;
}

export interface ShogiMatchProps {
    engineOptions: EngineOption[];
    defaultSides?: { sente: SideSetting; gote: SideSetting };
    initialMainTimeMs?: number;
    initialByoyomiMs?: number;
    maxLogs?: number;
    fetchLegalMoves?: (moves: string[]) => Promise<string[]>;
}

const HAND_ORDER: PieceType[] = ["R", "B", "G", "S", "N", "L", "P"];
const PIECE_LABELS: Record<PieceType, string> = {
    K: "玉",
    R: "飛",
    B: "角",
    G: "金",
    S: "銀",
    N: "桂",
    L: "香",
    P: "歩",
};

const baseCard: CSSProperties = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "14px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
};

function formatEvent(event: EngineEvent, engineId: string): string {
    const prefix = `[${engineId}] `;
    if (event.type === "bestmove") {
        return (
            prefix +
            (event.ponder
                ? `bestmove ${event.move} (ponder ${event.ponder})`
                : `bestmove ${event.move}`)
        );
    }
    if (event.type === "info") {
        const score =
            event.scoreMate !== undefined
                ? `mate ${event.scoreMate}`
                : event.scoreCp !== undefined
                  ? `cp ${event.scoreCp}`
                  : "";
        return (
            prefix +
            [
                `info depth ${event.depth ?? "-"}`,
                event.nodes !== undefined ? `nodes ${event.nodes}` : null,
                event.nps !== undefined ? `nps ${event.nps}` : null,
                score ? `score ${score}` : null,
            ]
                .filter(Boolean)
                .join(" ")
        );
    }
    return `${prefix}error ${event.message}`;
}

function formatTime(ms: number): string {
    if (ms < 0) ms = 0;
    const totalSeconds = Math.floor(ms / 1000);
    const minutes = Math.floor(totalSeconds / 60)
        .toString()
        .padStart(2, "0");
    const seconds = (totalSeconds % 60).toString().padStart(2, "0");
    return `${minutes}:${seconds}`;
}

function boardToGrid(board: BoardState): ShogiBoardCell[][] {
    const matrix = boardToMatrix(board);
    return matrix.map((row) =>
        row.map((cell) => ({
            id: cell.square,
            piece: cell.piece
                ? {
                      owner: cell.piece.owner,
                      type: cell.piece.type,
                      promoted: cell.piece.promoted,
                  }
                : null,
        })),
    );
}

function initialTick(settings: ClockSettings): TickState {
    return {
        sente: { mainMs: settings.sente.mainMs, byoyomiMs: settings.sente.byoyomiMs },
        gote: { mainMs: settings.gote.mainMs, byoyomiMs: settings.gote.byoyomiMs },
        ticking: null,
        lastUpdatedAt: Date.now(),
    };
}

export function ShogiMatch({
    engineOptions,
    defaultSides = {
        sente: { role: "human" },
        gote: { role: "engine", engineId: engineOptions[0]?.id },
    },
    initialMainTimeMs = 0,
    initialByoyomiMs = 5_000,
    maxLogs = 80,
    fetchLegalMoves,
}: ShogiMatchProps): ReactElement {
    const hasEngines = engineOptions.length > 0;
    const emptyBoard = useMemo<BoardState>(
        () => Object.fromEntries(getAllSquares().map((sq) => [sq, null])) as BoardState,
        [],
    );
    const [sides, setSides] = useState<{ sente: SideSetting; gote: SideSetting }>(defaultSides);
    const [position, setPosition] = useState<PositionState>({
        board: emptyBoard,
        hands: createEmptyHands(),
        turn: "sente",
        ply: 1,
    });
    const [initialBoard, setInitialBoard] = useState<BoardState | null>(null);
    const [positionReady, setPositionReady] = useState(false);
    const [moves, setMoves] = useState<string[]>([]);
    const [lastMove, setLastMove] = useState<LastMove | undefined>(undefined);
    const [selection, setSelection] = useState<Selection | null>(null);
    const [wantPromote, setWantPromote] = useState(false);
    const [engineReady, setEngineReady] = useState<Record<string, boolean>>({});
    const [engineStatus, setEngineStatus] = useState<Record<string, EngineStatus>>({});
    const [message, setMessage] = useState<string | null>(null);
    const [flipBoard, setFlipBoard] = useState(false);
    const [timeSettings, setTimeSettings] = useState<ClockSettings>({
        sente: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
        gote: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
    });
    const [clocks, setClocks] = useState<TickState>(initialTick(timeSettings));
    const [eventLogs, setEventLogs] = useState<string[]>([]);
    const [errorLogs, setErrorLogs] = useState<string[]>([]);
    const [activeSearch, setActiveSearch] = useState<{ side: Player; engineId: string } | null>(
        null,
    );
    const [isMatchRunning, setIsMatchRunning] = useState(false);

    const handlesRef = useRef<Record<string, SearchHandle | null>>({});
    const pendingSearchRef = useRef<Record<string, boolean>>({});
    const lastEngineRequestPly = useRef<number | null>(null);
    const positionRef = useRef<PositionState>(position);
    const movesRef = useRef<string[]>(moves);
    const legalCacheRef = useRef<{ ply: number; moves: Set<string> } | null>(null);

    useEffect(() => {
        const service = getPositionService();
        service
            .getInitialBoard()
            .then((pos) => {
                setPosition(pos);
                positionRef.current = pos;
                setInitialBoard(cloneBoard(pos.board));
                setPositionReady(true);
            })
            .catch((error) => setMessage(`初期局面の取得に失敗しました: ${String(error)}`));
    }, []);

    const engineMap = useMemo(() => {
        const map = new Map<string, EngineOption>();
        for (const opt of engineOptions) {
            map.set(opt.id, opt);
        }
        return map;
    }, [engineOptions]);

    const grid = useMemo(() => {
        const g = boardToGrid(position.board);
        return flipBoard ? [...g].reverse().map((row) => [...row].reverse()) : g;
    }, [position.board, flipBoard]);

    const getEngineForSide = (side: Player): EngineOption | undefined => {
        if (!hasEngines) return undefined;
        const setting = sides[side];
        if (setting.role !== "engine") return undefined;
        if (setting.engineId && engineMap.has(setting.engineId)) {
            return engineMap.get(setting.engineId);
        }
        return engineOptions[0];
    };

    const isEngineTurn = (side: Player): boolean => {
        return sides[side].role === "engine" && Boolean(getEngineForSide(side));
    };

    const ensureEngineReady = useCallback(
        async (engineId: string, engine: EngineClient) => {
            if (engineReady[engineId]) return;
            await engine.init();
            await engine.loadPosition("startpos");
            setEngineReady((prev) => ({ ...prev, [engineId]: true }));
        },
        [engineReady],
    );

    const resetClocks = (startTick: boolean) => {
        setClocks({
            sente: { mainMs: timeSettings.sente.mainMs, byoyomiMs: timeSettings.sente.byoyomiMs },
            gote: { mainMs: timeSettings.gote.mainMs, byoyomiMs: timeSettings.gote.byoyomiMs },
            ticking: startTick ? "sente" : null,
            lastUpdatedAt: Date.now(),
        });
    };

    const stopAllEngines = useCallback(async () => {
        const entries = Object.entries(handlesRef.current);
        for (const [, handle] of entries) {
            if (handle) {
                await handle.cancel().catch(() => undefined);
            }
        }
        handlesRef.current = {};
        pendingSearchRef.current = {};
        setActiveSearch(null);
        setEngineStatus({});
    }, []);

    const pauseAutoPlay = async () => {
        setIsMatchRunning(false);
        setClocks((prev) => ({ ...prev, ticking: null }));
        await stopAllEngines();
    };

    const resumeAutoPlay = async () => {
        if (!positionReady) return;
        setIsMatchRunning(true);
        setClocks((prev) => ({ ...prev, ticking: position.turn, lastUpdatedAt: Date.now() }));
        if (!isEngineTurn(position.turn)) return;
        const engineOpt = getEngineForSide(position.turn);
        if (engineOpt) {
            try {
                await startEngineTurn(position.turn, engineOpt.id);
            } catch (error) {
                setEngineStatus((prev) => ({ ...prev, [engineOpt.id]: "error" }));
                setErrorLogs((prev) =>
                    [`engine error: ${String(error)}`, ...prev].slice(0, maxLogs),
                );
            }
        }
    };

    const updateClocksForNextTurn = useCallback(
        (nextTurn: Player) => {
            setClocks((prev) => ({
                ...prev,
                [nextTurn]: {
                    mainMs: prev[nextTurn].mainMs,
                    byoyomiMs: timeSettings[nextTurn].byoyomiMs,
                },
                ticking: nextTurn,
                lastUpdatedAt: Date.now(),
            }));
        },
        [timeSettings],
    );

    const applyMoveCommon = useCallback(
        (nextPosition: PositionState, mv: string, last?: LastMove) => {
            setPosition(nextPosition);
            setMoves((prev) => [...prev, mv]);
            setLastMove(last);
            setSelection(null);
            setWantPromote(false);
            setMessage(null);
            setActiveSearch(null);
            legalCacheRef.current = null;
            updateClocksForNextTurn(nextPosition.turn);
        },
        [updateClocksForNextTurn],
    );

    const applyMoveFromEngine = useCallback(
        (move: string) => {
            const result = applyMoveWithState(positionRef.current, move, { validateTurn: false });
            if (!result.ok) {
                setErrorLogs((prev) =>
                    [`engine move rejected: ${result.error ?? "unknown"}`, ...prev].slice(
                        0,
                        maxLogs,
                    ),
                );
                return;
            }
            applyMoveCommon(result.next, move, result.lastMove);
        },
        [applyMoveCommon, maxLogs],
    );

    const startEngineTurn = useCallback(
        async (side: Player, engineId: string) => {
            if (!positionReady) return;
            if (pendingSearchRef.current[engineId]) return;
            const engine = engineMap.get(engineId);
            if (!engine) return;
            const existing = handlesRef.current[engineId];
            if (existing) {
                // 既にこのエンジンが思考中ならそのまま継続（StrictModeで二重発火するのを防ぐ）
                if (activeSearch && activeSearch.engineId === engineId) {
                    return;
                }
                await existing.cancel().catch(() => undefined);
            }
            setEngineStatus((prev) => ({ ...prev, [engineId]: "thinking" }));
            pendingSearchRef.current[engineId] = true;
            await ensureEngineReady(engineId, engine.client);
            await engine.client.loadPosition("startpos", movesRef.current);
            const handle = await engine.client.search({
                limits: { byoyomiMs: timeSettings[side].byoyomiMs },
                ponder: false,
            });
            handlesRef.current[engineId] = handle;
            setActiveSearch({ side, engineId });
            pendingSearchRef.current[engineId] = false;
        },
        [activeSearch, engineMap, ensureEngineReady, positionReady, timeSettings],
    );

    useEffect(() => {
        positionRef.current = position;
    }, [position]);

    useEffect(() => {
        movesRef.current = moves;
    }, [moves]);

    useEffect(() => {
        for (const side of ["sente", "gote"] as Player[]) {
            const setting = sides[side];
            if (setting.role === "engine") continue;
            const fallbackEngine = engineOptions[0];
            const engineOpt = setting.engineId ? engineMap.get(setting.engineId) : fallbackEngine;
            if (!engineOpt) continue;
            const handle = handlesRef.current[engineOpt.id];
            if (handle) {
                handle.cancel().catch(() => undefined);
                handlesRef.current[engineOpt.id] = null;
            }
            if (activeSearch?.side === side) {
                setActiveSearch(null);
            }
        }
    }, [activeSearch, engineMap, engineOptions, sides]);

    useEffect(() => {
        if (!hasEngines) return;
        const unsubs = engineOptions.map(({ id, client }) =>
            client.subscribe((event) => {
                setEventLogs((prev) => {
                    const next = [formatEvent(event, id), ...prev];
                    return next.length > maxLogs ? next.slice(0, maxLogs) : next;
                });
                if (event.type === "bestmove") {
                    setEngineStatus((prev) => ({ ...prev, [id]: "idle" }));
                    // 探索完了したハンドルはクリアして次回のキャンセルでstopを送らないようにする
                    if (handlesRef.current[id]) {
                        handlesRef.current[id] = null;
                    }
                    if (activeSearch && activeSearch.engineId === id) {
                        // 次の手番で再探索できるよう、現在の手数を記録
                        lastEngineRequestPly.current = movesRef.current.length;
                        pendingSearchRef.current[id] = false;
                        applyMoveFromEngine(event.move);
                        setActiveSearch(null);
                    }
                }
                if (event.type === "error") {
                    setEngineStatus((prev) => ({ ...prev, [id]: "error" }));
                    if (handlesRef.current[id]) {
                        handlesRef.current[id] = null;
                    }
                    pendingSearchRef.current[id] = false;
                    setErrorLogs((prev) => [event.message, ...prev].slice(0, maxLogs));
                }
            }),
        );

        return () => {
            for (const unsubscribe of unsubs) {
                unsubscribe();
            }
        };
    }, [activeSearch, applyMoveFromEngine, engineOptions, hasEngines, maxLogs]);

    useEffect(() => {
        return () => {
            // アンマウント時のみ全エンジンを停止する。再レンダー間のcleanupでは stop を送らない。
            stopAllEngines().catch(() => undefined);
        };
    }, [stopAllEngines]);

    useEffect(() => {
        if (!clocks.ticking) return;
        const timer = setInterval(() => {
            setClocks((prev) => {
                if (!prev.ticking) return prev;
                const now = Date.now();
                const delta = now - prev.lastUpdatedAt;
                const side = prev.ticking;
                const current = prev[side];
                let mainMs = current.mainMs - delta;
                let byoyomiMs = current.byoyomiMs;
                if (mainMs < 0) {
                    const over = Math.abs(mainMs);
                    mainMs = 0;
                    byoyomiMs = Math.max(0, byoyomiMs - over);
                }
                return {
                    ...prev,
                    [side]: { mainMs: Math.max(0, mainMs), byoyomiMs },
                    lastUpdatedAt: now,
                };
            });
        }, 200);
        return () => clearInterval(timer);
    }, [clocks.ticking]);

    useEffect(() => {
        if (!isMatchRunning || !hasEngines || !positionReady) return;
        const setting = sides[position.turn];
        if (setting.role !== "engine") return;
        const fallbackEngine = engineOptions[0];
        const engineOpt = setting.engineId
            ? (engineMap.get(setting.engineId) ?? fallbackEngine)
            : fallbackEngine;
        if (!engineOpt) return;
        if (
            activeSearch &&
            activeSearch.side === position.turn &&
            activeSearch.engineId === engineOpt.id
        ) {
            return;
        }
        if (lastEngineRequestPly.current === moves.length) return;
        lastEngineRequestPly.current = moves.length;
        startEngineTurn(position.turn, engineOpt.id).catch((error) => {
            setEngineStatus((prev) => ({ ...prev, [engineOpt.id]: "error" }));
            setErrorLogs((prev) => [`engine error: ${String(error)}`, ...prev].slice(0, maxLogs));
        });
    }, [
        activeSearch,
        isMatchRunning,
        engineMap,
        engineOptions,
        hasEngines,
        maxLogs,
        moves.length,
        position.turn,
        positionReady,
        sides,
        startEngineTurn,
    ]);

    const handleNewGame = async () => {
        await stopAllEngines();
        const initial = await getPositionService().getInitialBoard();
        setPosition(initial);
        positionRef.current = initial;
        setInitialBoard(cloneBoard(initial.board));
        setPositionReady(true);
        setMoves([]);
        setLastMove(undefined);
        setSelection(null);
        setMessage(null);
        resetClocks(false);
        lastEngineRequestPly.current = null;
        setActiveSearch(null);
        setIsMatchRunning(false);
        legalCacheRef.current = null;
    };

    const getLegalSet = async (): Promise<Set<string> | null> => {
        if (!positionReady) return null;
        const resolver =
            fetchLegalMoves ??
            (async (history: string[]) => getPositionService().getLegalMoves("startpos", history));
        const ply = movesRef.current.length;
        const cached = legalCacheRef.current;
        if (cached && cached.ply === ply) {
            return cached.moves;
        }
        const list = await resolver(movesRef.current);
        const set = new Set(list);
        legalCacheRef.current = { ply, moves: set };
        return set;
    };

    const handleSquareSelect = async (square: string) => {
        setMessage(null);
        if (!positionReady) {
            setMessage("局面を読み込み中です。");
            return;
        }
        if (isEngineTurn(position.turn)) {
            setMessage("エンジンの手番です。");
            return;
        }
        if (!selection) {
            const sq = square as Square;
            const piece = position.board[sq];
            if (piece && piece.owner === position.turn) {
                setSelection({ kind: "square", square: sq });
            }
            return;
        }

        if (selection.kind === "square") {
            if (selection.square === square) {
                setSelection(null);
                return;
            }
            const moveStr = `${selection.square}${square}${wantPromote ? "+" : ""}`;
            const legal = await getLegalSet();
            if (legal && !legal.has(moveStr)) {
                setMessage("合法手ではありません");
                return;
            }
            const result = applyMoveWithState(position, moveStr, { validateTurn: true });
            if (!result.ok) {
                setMessage(result.error ?? "指し手を適用できませんでした");
                return;
            }
            applyMoveCommon(result.next, moveStr, result.lastMove);
            return;
        }

        const moveStr = `${selection.piece}*${square}`;
        const legal = await getLegalSet();
        if (legal && !legal.has(moveStr)) {
            setMessage("合法手ではありません");
            return;
        }
        const result = applyMoveWithState(position, moveStr, { validateTurn: true });
        if (!result.ok) {
            setMessage(result.error ?? "持ち駒を打てませんでした");
            return;
        }
        applyMoveCommon(result.next, moveStr, result.lastMove);
    };

    const handleHandSelect = (piece: PieceType) => {
        if (!positionReady) {
            setMessage("局面を読み込み中です。");
            return;
        }
        if (isEngineTurn(position.turn)) {
            setMessage("エンジンの手番です。");
            return;
        }
        setSelection({ kind: "hand", piece });
        setMessage(null);
    };

    const deriveLastMove = (move: string | undefined): LastMove | undefined => {
        const parsed = move ? parseMove(move) : null;
        if (!parsed) return undefined;
        if (parsed.kind === "drop") {
            return { from: null, to: parsed.to, dropPiece: parsed.piece, promotes: false };
        }
        return { from: parsed.from, to: parsed.to, promotes: parsed.promote };
    };

    const importUsi = async (raw: string) => {
        const trimmed = raw.trim();
        if (!trimmed) return;
        const tokens = trimmed.includes("moves")
            ? (trimmed.split("moves")[1]?.trim().split(/\s+/) ?? [])
            : trimmed.split(/\s+/);
        await loadMoves(tokens);
    };

    const loadMoves = async (list: string[]) => {
        const filtered = list.filter(Boolean);
        const service = getPositionService();
        try {
            const result = await service.replayMovesStrict("startpos", filtered);
            setPosition(result.position);
            positionRef.current = result.position;
            setMoves(result.applied);
            setLastMove(deriveLastMove(result.applied.at(-1)));
            setSelection(null);
            setMessage(result.error ?? null);
            resetClocks(true);
            lastEngineRequestPly.current = null;
            legalCacheRef.current = null;
            setPositionReady(true);
        } catch (error) {
            setMessage(`棋譜の適用に失敗しました: ${String(error)}`);
        }
    };

    const exportUsi = buildPositionString(moves);
    const exportCsa = useMemo(
        () => (positionReady && initialBoard ? movesToCsa(moves, {}, initialBoard) : ""),
        [initialBoard, moves, positionReady],
    );

    const handView = (owner: Player) => {
        const hand = position.hands[owner];
        const ownerSetting = sides[owner];
        const isActive = position.turn === owner && ownerSetting.role === "human";
        return (
            <div style={{ display: "flex", flexWrap: "wrap", gap: "6px" }}>
                {HAND_ORDER.map((piece) => {
                    const count = hand[piece] ?? 0;
                    const selected = selection?.kind === "hand" && selection.piece === piece;
                    return (
                        <button
                            key={`${owner}-${piece}`}
                            type="button"
                            onClick={() => handleHandSelect(piece)}
                            disabled={count <= 0 || !isActive}
                            style={{
                                minWidth: "52px",
                                padding: "6px 10px",
                                borderRadius: "10px",
                                border: selected
                                    ? "2px solid hsl(var(--primary, 15 86% 55%))"
                                    : "1px solid hsl(var(--border, 0 0% 86%))",
                                background:
                                    count > 0
                                        ? "hsl(var(--secondary, 210 40% 96%))"
                                        : "transparent",
                                color: "hsl(var(--foreground, 222 47% 11%))",
                                cursor: count > 0 && isActive ? "pointer" : "not-allowed",
                            }}
                        >
                            {PIECE_LABELS[piece]} × {count}
                        </button>
                    );
                })}
            </div>
        );
    };

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

    const candidateNote = positionReady
        ? "合法手はRustエンジンの結果に基づいています。"
        : "局面を読み込み中です。";

    const sideSelector = (side: Player) => {
        const setting = sides[side];
        const engineList = engineOptions.map((opt) => (
            <option key={opt.id} value={opt.id}>
                {opt.label}
            </option>
        ));
        return (
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "6px" }}>
                <label
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: "4px",
                        fontSize: "13px",
                    }}
                >
                    {side === "sente" ? "先手" : "後手"} の操作
                    <select
                        value={setting.role}
                        onChange={(e) =>
                            setSides((prev) => ({
                                ...prev,
                                [side]: {
                                    ...prev[side],
                                    role: e.target.value as SideRole,
                                    engineId: prev[side].engineId ?? engineOptions[0]?.id,
                                },
                            }))
                        }
                        style={{
                            padding: "8px",
                            borderRadius: "8px",
                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                        }}
                    >
                        <option value="human">人間</option>
                        <option value="engine">エンジン</option>
                    </select>
                </label>
                <label
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: "4px",
                        fontSize: "13px",
                    }}
                >
                    <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
                        <span>使用するエンジン</span>
                        <Tooltip>
                            <TooltipTrigger asChild>
                                <span
                                    role="img"
                                    aria-label="内蔵エンジンの補足"
                                    style={{
                                        display: "inline-flex",
                                        alignItems: "center",
                                        justifyContent: "center",
                                        width: "18px",
                                        height: "18px",
                                        borderRadius: "999px",
                                        border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        background: "hsl(var(--card, 0 0% 100%))",
                                        color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                        fontSize: "11px",
                                        cursor: "default",
                                        lineHeight: 1,
                                    }}
                                >
                                    i
                                </span>
                            </TooltipTrigger>
                            <TooltipContent side="top">
                                A/B
                                は同じ内蔵エンジンへの別クライアントです。先手/後手などに割り当てるためのスロットです。
                            </TooltipContent>
                        </Tooltip>
                    </div>
                    <select
                        value={setting.engineId ?? engineOptions[0]?.id}
                        onChange={(e) =>
                            setSides((prev) => ({
                                ...prev,
                                [side]: { ...prev[side], engineId: e.target.value },
                            }))
                        }
                        disabled={setting.role !== "engine" || engineOptions.length === 0}
                        style={{
                            padding: "8px",
                            borderRadius: "8px",
                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                        }}
                    >
                        {engineList}
                    </select>
                </label>
            </div>
        );
    };

    return (
        <TooltipProvider delayDuration={120}>
            <section
                style={{
                    ...baseCard,
                    padding: "16px",
                    display: "flex",
                    flexDirection: "column",
                    gap: "12px",
                }}
            >
                <div
                    style={{
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                        gap: "12px",
                    }}
                >
                    <div>
                        <div style={{ fontWeight: 700 }}>盤 UI + 対局</div>
                        <div
                            style={{
                                color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                fontSize: "13px",
                            }}
                        >
                            先手・後手それぞれに「人間 /
                            エンジン」を割り当てて試せます。クリック2回で移動、持ち駒はボタン→マスで打ち込み。
                        </div>
                    </div>
                    <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
                        <label
                            style={{
                                display: "flex",
                                alignItems: "center",
                                gap: "6px",
                                fontSize: "13px",
                            }}
                        >
                            <input
                                type="checkbox"
                                checked={flipBoard}
                                onChange={(e) => setFlipBoard(e.target.checked)}
                            />
                            盤面を反転
                        </label>
                    </div>
                </div>

                <div
                    style={{
                        display: "grid",
                        gridTemplateColumns: "minmax(320px, 1fr) 360px",
                        gap: "12px",
                        alignItems: "flex-start",
                    }}
                >
                    <div style={{ display: "flex", flexDirection: "column", gap: "12px" }}>
                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div
                                style={{
                                    display: "flex",
                                    justifyContent: "space-between",
                                    alignItems: "center",
                                }}
                            >
                                <div style={{ fontWeight: 700 }}>盤面</div>
                                <label
                                    style={{
                                        display: "flex",
                                        alignItems: "center",
                                        gap: "6px",
                                        fontSize: "13px",
                                    }}
                                >
                                    <input
                                        type="checkbox"
                                        checked={wantPromote}
                                        onChange={(e) => setWantPromote(e.target.checked)}
                                    />
                                    成りにする
                                </label>
                            </div>
                            <div
                                style={{
                                    marginTop: "8px",
                                    display: "flex",
                                    gap: "8px",
                                    flexDirection: "column",
                                }}
                            >
                                <ShogiBoard
                                    grid={grid}
                                    selectedSquare={
                                        selection?.kind === "square" ? selection.square : null
                                    }
                                    lastMove={
                                        lastMove
                                            ? { from: lastMove.from ?? undefined, to: lastMove.to }
                                            : undefined
                                    }
                                    onSelect={(sq) => {
                                        void handleSquareSelect(sq);
                                    }}
                                />
                                <div
                                    style={{
                                        fontSize: "12px",
                                        color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                    }}
                                >
                                    {candidateNote}
                                </div>
                            </div>
                        </div>

                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div
                                style={{
                                    display: "flex",
                                    justifyContent: "space-between",
                                    alignItems: "center",
                                }}
                            >
                                <div style={{ fontWeight: 700 }}>先手の持ち駒</div>
                                <div
                                    style={{
                                        fontSize: "12px",
                                        color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                    }}
                                >
                                    手番: {position.turn === "sente" ? "先手" : "後手"}
                                </div>
                            </div>
                            <div style={{ marginTop: "8px" }}>{handView("sente")}</div>
                            <div style={{ marginTop: "14px", fontWeight: 700 }}>後手の持ち駒</div>
                            <div style={{ marginTop: "8px" }}>{handView("gote")}</div>
                        </div>
                    </div>

                    <div style={{ display: "flex", flexDirection: "column", gap: "10px" }}>
                        <div
                            style={{
                                ...baseCard,
                                padding: "12px",
                                display: "flex",
                                flexDirection: "column",
                                gap: "10px",
                            }}
                        >
                            <div style={{ fontWeight: 700 }}>対局設定</div>
                            {sideSelector("sente")}
                            {sideSelector("gote")}

                            <div
                                style={{
                                    display: "grid",
                                    gridTemplateColumns: "1fr 1fr",
                                    gap: "8px",
                                }}
                            >
                                <label
                                    htmlFor="sente-main"
                                    style={{
                                        display: "flex",
                                        flexDirection: "column",
                                        gap: "4px",
                                        fontSize: "13px",
                                    }}
                                >
                                    先手 持ち時間 (ms)
                                    <Input
                                        id="sente-main"
                                        type="number"
                                        value={timeSettings.sente.mainMs}
                                        onChange={(e) =>
                                            setTimeSettings((prev) => ({
                                                ...prev,
                                                sente: {
                                                    ...prev.sente,
                                                    mainMs: Number(e.target.value),
                                                },
                                            }))
                                        }
                                    />
                                </label>
                                <label
                                    htmlFor="sente-byoyomi"
                                    style={{
                                        display: "flex",
                                        flexDirection: "column",
                                        gap: "4px",
                                        fontSize: "13px",
                                    }}
                                >
                                    先手 秒読み (ms)
                                    <Input
                                        id="sente-byoyomi"
                                        type="number"
                                        value={timeSettings.sente.byoyomiMs}
                                        onChange={(e) =>
                                            setTimeSettings((prev) => ({
                                                ...prev,
                                                sente: {
                                                    ...prev.sente,
                                                    byoyomiMs: Number(e.target.value),
                                                },
                                            }))
                                        }
                                    />
                                </label>
                                <label
                                    htmlFor="gote-main"
                                    style={{
                                        display: "flex",
                                        flexDirection: "column",
                                        gap: "4px",
                                        fontSize: "13px",
                                    }}
                                >
                                    後手 持ち時間 (ms)
                                    <Input
                                        id="gote-main"
                                        type="number"
                                        value={timeSettings.gote.mainMs}
                                        onChange={(e) =>
                                            setTimeSettings((prev) => ({
                                                ...prev,
                                                gote: {
                                                    ...prev.gote,
                                                    mainMs: Number(e.target.value),
                                                },
                                            }))
                                        }
                                    />
                                </label>
                                <label
                                    htmlFor="gote-byoyomi"
                                    style={{
                                        display: "flex",
                                        flexDirection: "column",
                                        gap: "4px",
                                        fontSize: "13px",
                                    }}
                                >
                                    後手 秒読み (ms)
                                    <Input
                                        id="gote-byoyomi"
                                        type="number"
                                        value={timeSettings.gote.byoyomiMs}
                                        onChange={(e) =>
                                            setTimeSettings((prev) => ({
                                                ...prev,
                                                gote: {
                                                    ...prev.gote,
                                                    byoyomiMs: Number(e.target.value),
                                                },
                                            }))
                                        }
                                    />
                                </label>
                            </div>

                            <div
                                style={{
                                    display: "flex",
                                    gap: "8px",
                                    flexWrap: "wrap",
                                    alignItems: "center",
                                }}
                            >
                                <Button
                                    type="button"
                                    onClick={handleNewGame}
                                    style={{ paddingInline: "12px" }}
                                >
                                    新規対局（初期化）
                                </Button>
                                <Button
                                    type="button"
                                    onClick={pauseAutoPlay}
                                    variant="outline"
                                    style={{ paddingInline: "12px" }}
                                >
                                    停止（自動進行オフ）
                                </Button>
                                <Button
                                    type="button"
                                    onClick={resumeAutoPlay}
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
                                {engineOptions.map((opt) => {
                                    const status = engineStatus[opt.id] ?? "idle";
                                    const ready = engineReady[opt.id] ? "init済" : "未init";
                                    return ` [${opt.label}: ${status}/${ready}]`;
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

                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div style={{ fontWeight: 700, marginBottom: "6px" }}>時計</div>
                            <div style={{ display: "flex", flexDirection: "column", gap: "6px" }}>
                                {renderClock("sente")}
                                {renderClock("gote")}
                            </div>
                        </div>

                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div style={{ fontWeight: 700, marginBottom: "6px" }}>
                                棋譜 / 入出力
                            </div>
                            <div
                                style={{
                                    fontSize: "13px",
                                    color: "hsl(var(--muted-foreground, 0 0% 48%))",
                                }}
                            >
                                先手から {moves.length} 手目
                            </div>
                            <ol
                                style={{
                                    paddingLeft: "18px",
                                    maxHeight: "160px",
                                    overflow: "auto",
                                    margin: "8px 0",
                                }}
                            >
                                {moves.map((mv, idx) => (
                                    <li
                                        key={`${idx}-${mv}`}
                                        style={{
                                            fontFamily: "ui-monospace, monospace",
                                            fontSize: "13px",
                                        }}
                                    >
                                        {idx + 1}. {mv}
                                    </li>
                                ))}
                            </ol>
                            <div
                                style={{ display: "grid", gridTemplateColumns: "1fr", gap: "8px" }}
                            >
                                <label
                                    style={{ display: "flex", flexDirection: "column", gap: "4px" }}
                                >
                                    USI / SFEN (startpos moves)
                                    <textarea
                                        value={exportUsi}
                                        onChange={(e) => {
                                            void importUsi(e.target.value);
                                        }}
                                        rows={3}
                                        style={{
                                            width: "100%",
                                            padding: "8px",
                                            borderRadius: "8px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    />
                                </label>
                                <label
                                    style={{ display: "flex", flexDirection: "column", gap: "4px" }}
                                >
                                    CSA
                                    <textarea
                                        value={exportCsa}
                                        onChange={(e) => {
                                            if (!positionReady) return;
                                            void loadMoves(
                                                parseCsaMoves(
                                                    e.target.value,
                                                    initialBoard ?? undefined,
                                                ),
                                            );
                                        }}
                                        rows={3}
                                        style={{
                                            width: "100%",
                                            padding: "8px",
                                            borderRadius: "8px",
                                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                                        }}
                                    />
                                </label>
                            </div>
                        </div>

                        <div style={{ ...baseCard, padding: "12px" }}>
                            <div style={{ fontWeight: 700, marginBottom: "6px" }}>エンジンログ</div>
                            <ul
                                style={{
                                    listStyle: "none",
                                    padding: 0,
                                    margin: 0,
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: "4px",
                                    maxHeight: "160px",
                                    overflow: "auto",
                                }}
                            >
                                {eventLogs.map((log, idx) => (
                                    <li
                                        key={`${idx}-${log}`}
                                        style={{
                                            fontFamily: "ui-monospace, monospace",
                                            fontSize: "12px",
                                        }}
                                    >
                                        {log}
                                    </li>
                                ))}
                            </ul>
                            {errorLogs.length ? (
                                <div
                                    style={{
                                        marginTop: "8px",
                                        color: "hsl(var(--destructive, 0 72% 51%))",
                                        fontSize: "12px",
                                    }}
                                >
                                    {errorLogs[0]}
                                </div>
                            ) : null}
                        </div>
                    </div>
                </div>
            </section>
        </TooltipProvider>
    );
}
