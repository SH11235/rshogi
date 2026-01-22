import {
    applyMoveWithState,
    type BoardState,
    boardToMatrix,
    canPass,
    cloneBoard,
    createEmptyHands,
    type GameResult,
    getAllSquares,
    getPathToNode,
    getPositionService,
    type LastMove,
    type Piece,
    type PieceType,
    type Player,
    type PositionState,
    parseMove,
    resolveWorkerCount,
    type Square,
} from "@shogi/app-core";
import type { ReactElement } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ShogiBoardCell } from "./shogi-board";
import { ShogiBoard } from "./shogi-board";
import { ClockDisplay } from "./shogi-match/components/ClockDisplay";
import { EngineLogsPanel } from "./shogi-match/components/EngineLogsPanel";
import { EvalPanel } from "./shogi-match/components/EvalPanel";
import { GameResultDialog } from "./shogi-match/components/GameResultDialog";
import { HandPiecesDisplay } from "./shogi-match/components/HandPiecesDisplay";
import { KifuImportPanel } from "./shogi-match/components/KifuImportPanel";
import { KifuPanel } from "./shogi-match/components/KifuPanel";
import { MatchControls } from "./shogi-match/components/MatchControls";
import {
    type EngineOption,
    MatchSettingsPanel,
    type SideSetting,
} from "./shogi-match/components/MatchSettingsPanel";
import { MoveDetailPanel } from "./shogi-match/components/MoveDetailPanel";
import { PassButton } from "./shogi-match/components/PassButton";
import { PassRightsDisplay } from "./shogi-match/components/PassRightsDisplay";
import { PvPreviewDialog } from "./shogi-match/components/PvPreviewDialog";
import { SettingsModal } from "./shogi-match/components/SettingsModal";
import { applyDropResult, DragGhost, type DropResult, usePieceDnd } from "./shogi-match/dnd";

// EngineOption 型を外部に再エクスポート
export type { EngineOption };

import { AppMenu } from "./shogi-match/components/AppMenu";
import { type ClockSettings, useClockManager } from "./shogi-match/hooks/useClockManager";
import { useEngineManager } from "./shogi-match/hooks/useEngineManager";
import { type AnalysisJob, useEnginePool } from "./shogi-match/hooks/useEnginePool";
import { useKifuKeyboardNavigation } from "./shogi-match/hooks/useKifuKeyboardNavigation";
import { useKifuNavigation } from "./shogi-match/hooks/useKifuNavigation";
import { useLocalStorage } from "./shogi-match/hooks/useLocalStorage";
import { useIsMobile } from "./shogi-match/hooks/useMediaQuery";
import { MobileLayout } from "./shogi-match/layouts/MobileLayout";
import {
    ANALYZING_STATE_NONE,
    type AnalysisSettings,
    type AnalyzingState,
    DEFAULT_ANALYSIS_SETTINGS,
    DEFAULT_DISPLAY_SETTINGS,
    DEFAULT_PASS_RIGHTS_SETTINGS,
    type DisplaySettings,
    type GameMode,
    type Message,
    type PassRightsSettings,
    type PromotionSelection,
} from "./shogi-match/types";
import {
    addToHand,
    cloneHandsState,
    consumeFromHand,
    countPieces,
} from "./shogi-match/utils/boardUtils";
import {
    collectBranchAnalysisJobs,
    collectTreeAnalysisJobs,
    getAllBranches,
} from "./shogi-match/utils/branchTreeUtils";
import { isPromotable, PIECE_CAP, PIECE_LABELS } from "./shogi-match/utils/constants";
import { exportToKifString, type KifMove } from "./shogi-match/utils/kifFormat";
import { type KifMoveData, parseSfen } from "./shogi-match/utils/kifParser";
import { LegalMoveCache } from "./shogi-match/utils/legalMoveCache";
import { determinePromotion } from "./shogi-match/utils/promotionLogic";
import { TooltipProvider } from "./tooltip";

type Selection = { kind: "square"; square: string } | { kind: "hand"; piece: PieceType };

export interface ShogiMatchProps {
    engineOptions: EngineOption[];
    defaultSides?: { sente: SideSetting; gote: SideSetting };
    initialMainTimeMs?: number;
    initialByoyomiMs?: number;
    maxLogs?: number;
    fetchLegalMoves?: (
        sfen: string,
        moves: string[],
        options?: { passRights?: { sente: number; gote: number } },
    ) => Promise<string[]>;
    /** 開発者モード（エンジンログパネルなどを表示） */
    isDevMode?: boolean;
}

// デフォルト値の定数
const DEFAULT_BYOYOMI_MS = 5_000; // デフォルト秒読み時間（5秒）
const DEFAULT_MAX_LOGS = 80; // ログ履歴の最大保持件数
const TOOLTIP_DELAY_DURATION_MS = 120; // ツールチップ表示遅延

/**
 * パス権設定と棋譜からgetLegalMovesのオプションを生成するヘルパー関数
 *
 * 注意: 棋譜に"pass"が含まれる場合は、設定が無効でもpassRightsを送る必要がある。
 * これは、Rust側でパス手を適用する際にパス権が必須なため。
 * （パス権有効で対局後に設定をOFFにした場合や、パス入り棋譜を読み込んだ場合など）
 */
function buildPassRightsOptionForLegalMoves(
    passRightsSettings: { enabled: boolean; initialCount: number } | undefined,
    moves: string[],
): { passRights?: { sente: number; gote: number } } {
    // 大文字小文字を区別せずにパス手を検出（parseMoveと同様）
    const hasPassInMoves = moves.some((m) => m.toLowerCase() === "pass");

    if (passRightsSettings?.enabled) {
        // 設定が有効: 初期値を使用
        return {
            passRights: {
                sente: passRightsSettings.initialCount,
                gote: passRightsSettings.initialCount,
            },
        };
    }

    if (hasPassInMoves) {
        // 設定は無効だが棋譜にpassが含まれる: 十分な数のパス権を設定
        // （各プレイヤーのパス回数の最大値を使用）
        let sentePassCount = 0;
        let gotePassCount = 0;
        let isSenteTurn = true; // 平手初期局面は先手番
        for (const move of moves) {
            if (move.toLowerCase() === "pass") {
                if (isSenteTurn) {
                    sentePassCount++;
                } else {
                    gotePassCount++;
                }
            }
            isSenteTurn = !isSenteTurn;
        }
        // 最低でも現在のパス数 + 1 を確保（追加パスの余地を残す）
        const minRights = Math.max(sentePassCount, gotePassCount) + 1;
        return {
            passRights: {
                sente: minRights,
                gote: minRights,
            },
        };
    }

    // 設定無効かつパスなし: passRights不要
    return {};
}

// レイアウト用Tailwindクラス
const matchLayoutClasses = "flex flex-col gap-2 items-center py-2";

// CSS変数は style 属性で設定（Tailwindでは表現できない）
const matchLayoutCssVars = {
    "--kifu-panel-max-h": "min(60vh, calc(100dvh - 320px))",
    "--kifu-panel-branch-max-h": "calc(var(--kifu-panel-max-h) - 40px)",
    "--shogi-cell-size": "44px",
} as React.CSSProperties;

// テキストスタイル用Tailwindクラス定数
const TEXT_CLASSES = {
    mutedSecondary: "text-xs text-muted-foreground",
    moveCount: "text-center text-sm font-semibold text-foreground my-2",
} as const;

// 持ち駒表示セクションコンポーネント
interface PlayerHandSectionProps {
    owner: Player;
    hand: PositionState["hands"]["sente"] | PositionState["hands"]["gote"];
    selectedPiece: PieceType | null;
    isActive: boolean;
    onHandSelect: (piece: PieceType) => void;
    /** DnD 用 PointerDown ハンドラ */
    onPiecePointerDown?: (owner: Player, pieceType: PieceType, e: React.PointerEvent) => void;
    /** 編集モードかどうか */
    isEditMode?: boolean;
    /** 対局中かどうか */
    isMatchRunning?: boolean;
    /** 持ち駒を増やす（編集モード用） */
    onIncrement?: (piece: PieceType) => void;
    /** 持ち駒を減らす（編集モード用） */
    onDecrement?: (piece: PieceType) => void;
    /** 盤面反転状態 */
    flipBoard?: boolean;
}

function PlayerHandSection({
    owner,
    hand,
    selectedPiece,
    isActive,
    onHandSelect,
    onPiecePointerDown,
    isEditMode,
    isMatchRunning,
    onIncrement,
    onDecrement,
    flipBoard,
}: PlayerHandSectionProps): ReactElement {
    return (
        <div data-zone={`hand-${owner}`} className="w-full">
            <HandPiecesDisplay
                owner={owner}
                hand={hand}
                selectedPiece={selectedPiece}
                isActive={isActive}
                onHandSelect={onHandSelect}
                onPiecePointerDown={onPiecePointerDown}
                isEditMode={isEditMode}
                isMatchRunning={isMatchRunning}
                onIncrement={onIncrement}
                onDecrement={onDecrement}
                flipBoard={flipBoard}
            />
        </div>
    );
}

const clonePositionState = (pos: PositionState): PositionState => ({
    board: cloneBoard(pos.board),
    hands: cloneHandsState(pos.hands),
    turn: pos.turn,
    ply: pos.ply,
});

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

/**
 * USI形式の指し手文字列から最終手情報を導出
 */
function deriveLastMove(move: string | undefined): LastMove | undefined {
    const parsed = move ? parseMove(move) : null;
    if (!parsed) return undefined;
    if (parsed.kind === "drop") {
        return { from: null, to: parsed.to, dropPiece: parsed.piece, promotes: false };
    }
    if (parsed.kind === "pass") {
        // パス手の場合は移動先なし
        return { isPass: true };
    }
    return { from: parsed.from, to: parsed.to, promotes: parsed.promote };
}

export function ShogiMatch({
    engineOptions,
    defaultSides = {
        sente: { role: "engine", engineId: engineOptions[0]?.id },
        gote: { role: "engine", engineId: engineOptions[0]?.id },
    },
    initialMainTimeMs = 0,
    initialByoyomiMs = DEFAULT_BYOYOMI_MS,
    maxLogs = DEFAULT_MAX_LOGS,
    fetchLegalMoves,
    isDevMode = false,
}: ShogiMatchProps): ReactElement {
    const emptyBoard = useMemo<BoardState>(
        () => Object.fromEntries(getAllSquares().map((sq) => [sq, null])) as BoardState,
        [],
    );
    const [sides, setSides] = useLocalStorage<{ sente: SideSetting; gote: SideSetting }>(
        "shogi-match-sides",
        defaultSides,
    );
    const [position, setPosition] = useState<PositionState>({
        board: emptyBoard,
        hands: createEmptyHands(),
        turn: "sente",
        ply: 1,
    });
    const [, setInitialBoard] = useState<BoardState | null>(null);
    const [positionReady, setPositionReady] = useState(false);
    const [lastMove, setLastMove] = useState<LastMove | undefined>(undefined);
    const [selection, setSelection] = useState<Selection | null>(null);
    const [promotionSelection, setPromotionSelection] = useState<PromotionSelection | null>(null);
    const [message, setMessage] = useState<Message | null>(null);
    const [gameResult, setGameResult] = useState<GameResult | null>(null);
    const [showResultDialog, setShowResultDialog] = useState(false);
    const [flipBoard, setFlipBoard] = useState(false);
    const [timeSettings, setTimeSettings] = useLocalStorage<ClockSettings>(
        "shogi-match-time-settings",
        {
            sente: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
            gote: { mainMs: initialMainTimeMs, byoyomiMs: initialByoyomiMs },
        },
    );
    const [isMatchRunning, setIsMatchRunning] = useState(false);
    const [isEditMode, setIsEditMode] = useState(true);
    const [isPaused, setIsPaused] = useState(false);
    // 検討モード: 編集モードでも対局中でも一時停止中でもない状態
    // 自由に棋譜を閲覧し、分岐を作成できる
    const isReviewMode = !isEditMode && !isMatchRunning && !isPaused;
    const [editOwner, setEditOwner] = useState<Player>("sente");
    const [editPieceType, setEditPieceType] = useState<PieceType | null>(null);
    const [editPromoted, setEditPromoted] = useState(false);
    const [editFromSquare, setEditFromSquare] = useState<Square | null>(null);
    const [editTool, setEditTool] = useState<"place" | "erase">("place");
    const [startSfen, setStartSfen] = useState<string>("startpos");
    // TODO: 将来的に局面編集機能の強化で使用予定
    const [_basePosition, setBasePosition] = useState<PositionState | null>(null);
    const [displaySettings, setDisplaySettings] = useLocalStorage<DisplaySettings>(
        "shogi-display-settings",
        DEFAULT_DISPLAY_SETTINGS,
    );
    // 解析設定（古いlocalStorageデータとの互換性のためデフォルト値とマージ）
    const [storedAnalysisSettings, setAnalysisSettings] = useLocalStorage<AnalysisSettings>(
        "shogi-analysis-settings",
        DEFAULT_ANALYSIS_SETTINGS,
    );
    const analysisSettings = useMemo(() => {
        const merged = { ...DEFAULT_ANALYSIS_SETTINGS, ...storedAnalysisSettings };
        // 旧設定 autoAnalyzeBranch からの移行処理
        // autoAnalyzeMode が未設定で autoAnalyzeBranch が存在する場合、マッピングする
        const stored = storedAnalysisSettings as unknown as Record<string, unknown>;
        if (!("autoAnalyzeMode" in stored) && "autoAnalyzeBranch" in stored) {
            merged.autoAnalyzeMode = stored.autoAnalyzeBranch ? "delayed" : "off";
        }
        return merged;
    }, [storedAnalysisSettings]);
    // パス権設定
    const [passRightsSettings, setPassRightsSettings] = useLocalStorage<PassRightsSettings>(
        "shogi-pass-rights-settings",
        DEFAULT_PASS_RIGHTS_SETTINGS,
    );
    // PVプレビュー用のstate
    const [pvPreview, setPvPreview] = useState<{
        open: boolean;
        ply: number;
        pv: string[];
        startPosition: PositionState;
        evalCp?: number;
        evalMate?: number;
    } | null>(null);
    // 解析状態（union型で相互排他的な状態を型レベルで保証）
    const [analyzingState, setAnalyzingState] = useState<AnalyzingState>(ANALYZING_STATE_NONE);
    // 一括解析の状態
    const [batchAnalysis, setBatchAnalysis] = useState<{
        isRunning: boolean;
        currentIndex: number;
        totalCount: number;
        targetPlies: number[];
        inProgress?: number[]; // 並列解析中の手番号
    } | null>(null);
    // 最後に追加された分岐の情報（KifuPanelが直接その分岐ビューに遷移するため）
    // nodeIdではなくply+firstMoveを使用（StrictModeでnodeIdが不整合になる問題を回避）
    const [lastAddedBranchInfo, setLastAddedBranchInfo] = useState<{
        ply: number;
        firstMove: string;
    } | null>(null);
    // 選択中の分岐ノードID（キーボードナビゲーション用）
    const [selectedBranchNodeId, setSelectedBranchNodeId] = useState<string | null>(null);
    // 選択中の手の詳細（右パネル表示用）
    const [selectedMoveDetail, setSelectedMoveDetail] = useState<{
        move: KifMove;
        position: PositionState;
    } | null>(null);
    // 設定モーダルの表示状態
    const [isSettingsModalOpen, setIsSettingsModalOpen] = useState(false);

    // positionRef を先に定義（コールバックで使用するため）
    const positionRef = useRef<PositionState>(position);
    // 編集操作のバージョンカウンター（非同期SFEN計算の競合状態を防止）
    const editVersionRef = useRef(0);

    // ナビゲーションからの局面変更コールバック（メモ化して安定した参照を維持）
    const handleNavigationPositionChange = useCallback(
        (newPosition: PositionState, lastMoveInfo?: { from?: string; to: string }) => {
            setPosition(newPosition);
            positionRef.current = newPosition;
            // ナビゲーションからのlastMove情報を反映
            if (lastMoveInfo) {
                setLastMove({
                    from: (lastMoveInfo.from ?? null) as Square | null,
                    to: lastMoveInfo.to as Square,
                    promotes: false, // ナビゲーションでは成り情報を追跡しない
                });
            } else {
                setLastMove(undefined);
            }
        },
        [],
    );

    // 棋譜ナビゲーション管理フック
    const navigation = useKifuNavigation({
        initialPosition: position,
        initialSfen: startSfen,
        onPositionChange: handleNavigationPositionChange,
    });

    // navigation.resetの参照をrefで保持（初期化useEffectで使用）
    // navigation オブジェクト全体は useKifuNavigation 内で再生成されるため、
    // reset メソッドのみを保持して不要な再実行を防ぐ
    const navigationResetRef = useRef(navigation.reset);
    navigationResetRef.current = navigation.reset;

    // 互換性用のmoves配列
    const moves = navigation.getMovesArray();

    // 棋譜＋評価値データ
    const {
        kifMoves,
        evalHistory,
        boardHistory,
        positionHistory,
        branchMarkers,
        recordEvalByPly,
        recordEvalByNodeId,
        addPvAsBranch,
    } = navigation;

    // 後手が人間の場合は盤面を反転して手前側に表示
    useEffect(() => {
        const goteIsHuman = sides.gote.role === "human";
        const senteIsHuman = sides.sente.role === "human";
        // 後手のみ人間、または両方人間で後手優先の場合は反転
        // （後手が人間かつ先手がエンジンの場合に反転）
        setFlipBoard(goteIsHuman && !senteIsHuman);
    }, [sides.sente.role, sides.gote.role]);

    // 持ち駒表示用のヘルパー関数（メモ化してMobileBoardSectionの再レンダリングを防ぐ）
    const getHandInfo = useCallback(
        (pos: "top" | "bottom") => {
            const owner: Player =
                pos === "top" ? (flipBoard ? "sente" : "gote") : flipBoard ? "gote" : "sente";
            // 検討モードでは手番の持ち駒を選択可能（対局設定に関係なく）
            const isActiveInReview = isReviewMode && position.turn === owner;
            const isActiveInMatch =
                !isEditMode &&
                !isReviewMode &&
                position.turn === owner &&
                sides[owner].role === "human";
            return {
                owner,
                hand: owner === "sente" ? position.hands.sente : position.hands.gote,
                isActive: isActiveInReview || isActiveInMatch,
            };
        },
        [flipBoard, isReviewMode, isEditMode, position.turn, position.hands, sides],
    );

    const movesRef = useRef<string[]>(moves);
    const legalCache = useMemo(() => new LegalMoveCache(), []);
    // movesRefをnavigationの変更に同期し、legalCacheをクリア
    useEffect(() => {
        movesRef.current = moves;
        // ナビゲーションで局面が変わったらキャッシュをクリア
        legalCache.clear();
    }, [moves, legalCache]);
    // パス権設定変更時にキャッシュもクリアするラッパー
    // （合法手にpassが含まれるかどうかが変わるため）
    const handlePassRightsSettingsChange = useCallback(
        (newSettings: PassRightsSettings) => {
            setPassRightsSettings(newSettings);
            legalCache.clear();
        },
        [setPassRightsSettings, legalCache],
    );
    const matchEndedRef = useRef(false);
    const boardSectionRef = useRef<HTMLDivElement>(null);
    const settingsLocked = isMatchRunning;
    // 現在のターン開始時刻（消費時間計算用）
    const turnStartTimeRef = useRef<number>(Date.now());

    // endMatch のための ref（循環依存を回避）
    const endMatchRef = useRef<((result: GameResult) => Promise<void>) | null>(null);

    const handleClockError = useCallback((text: string) => {
        setMessage({ text, type: "error" });
    }, []);

    const stopAllEnginesRef = useRef<() => Promise<void>>(async () => {});

    // 時計管理フックを使用
    const { clocks, clocksRef, resetClocks, updateClocksForNextTurn, stopTicking, startTicking } =
        useClockManager({
            timeSettings,
            isMatchRunning,
            onTimeExpired: async (side) => {
                const winner: Player = side === "sente" ? "gote" : "sente";
                const result: GameResult = {
                    winner,
                    reason: { kind: "time_expired", loser: side },
                    totalMoves: movesRef.current.length,
                };
                await endMatchRef.current?.(result);
            },
            matchEndedRef,
            onClockError: handleClockError,
        });

    // 対局前に timeSettings が変更されたら clocks を同期
    // （resetClocks は timeSettings に依存しているため、resetClocks の変更で検知可能）
    useEffect(() => {
        if (!isMatchRunning) {
            resetClocks(false);
        }
    }, [isMatchRunning, resetClocks]);

    // 対局終了処理（エンジン管理フックから呼ばれる）
    const endMatch = useCallback(
        async (result: GameResult) => {
            if (matchEndedRef.current) return;
            matchEndedRef.current = true;
            setGameResult(result);
            setShowResultDialog(true);
            setIsMatchRunning(false);
            stopTicking();
            try {
                await stopAllEnginesRef.current();
            } catch (error) {
                console.error("エンジン停止に失敗しました:", error);
                setMessage({
                    text: `対局終了処理でエンジン停止に失敗しました: ${String(error ?? "unknown")}`,
                    type: "error",
                });
            }
        },
        [stopTicking],
    );

    // endMatchRef を更新
    endMatchRef.current = endMatch;

    // 投了処理
    const handleResign = useCallback(async () => {
        const currentTurn = positionRef.current.turn;
        const result: GameResult = {
            winner: currentTurn === "sente" ? "gote" : "sente",
            reason: { kind: "resignation", loser: currentTurn },
            totalMoves: movesRef.current.length,
        };
        await endMatch(result);
    }, [endMatch]);

    // 待った処理（2手戻す：相手の手と自分の前の手を戻す）
    const handleUndo = useCallback(async () => {
        const moveCount = movesRef.current.length;
        if (moveCount === 0) return;

        // エンジンの思考を停止（旧局面のbestmoveが適用されるのを防ぐ）
        await stopAllEnginesRef.current();

        // 2手戻す（自分の前の手まで戻る）
        // ただし、1手しかない場合は1手だけ戻す
        const undoCount = moveCount >= 2 ? 2 : 1;

        for (let i = 0; i < undoCount; i++) {
            navigation.goBack();
        }
        movesRef.current = movesRef.current.slice(0, -undoCount);

        // 待った後の思考時間計測を新しく開始
        turnStartTimeRef.current = Date.now();
        // 秒読みをリセット（戻った局面の手番で時計を更新）
        updateClocksForNextTurn(positionRef.current.turn);
    }, [navigation, updateClocksForNextTurn]);

    const handleMoveFromEngineRef = useRef<(move: string) => void>(() => {});

    // 分岐解析用の状態をrefで追跡（コールバック内で最新値を参照するため）
    const analyzingStateRef = useRef<AnalyzingState>(ANALYZING_STATE_NONE);
    useEffect(() => {
        analyzingStateRef.current = analyzingState;

        return () => {
            // クリーンアップ時にrefをリセット
            analyzingStateRef.current = ANALYZING_STATE_NONE;
        };
    }, [analyzingState]);

    // 評価値更新コールバック（分岐解析にも対応）
    const handleEvalUpdate = useCallback(
        (ply: number, event: import("@shogi/engine-client").EngineInfoEvent) => {
            const state = analyzingStateRef.current;
            // 分岐解析中の場合はノードIDで保存
            if (state.type === "by-node-id") {
                recordEvalByNodeId(state.nodeId, event);
            } else {
                // 通常解析の場合はplyで保存
                recordEvalByPly(ply, event);
            }
        },
        [recordEvalByPly, recordEvalByNodeId],
    );

    // エンジン管理フックを使用
    const {
        eventLogs,
        errorLogs,
        stopAllEngines,
        isEngineTurn,
        logEngineError,
        isAnalyzing,
        analyzePosition,
        engineErrorDetails,
        retryEngine,
        isRetrying,
    } = useEngineManager({
        sides,
        engineOptions,
        timeSettings,
        clocksRef,
        startSfen,
        movesRef,
        positionRef,
        isMatchRunning,
        positionReady,
        passRightsSettings,
        onMoveFromEngine: (move) => handleMoveFromEngineRef.current(move),
        onMatchEnd: endMatch,
        onEvalUpdate: handleEvalUpdate,
        maxLogs,
    });
    stopAllEnginesRef.current = stopAllEngines;

    // 並列一括解析用のエンジンプール
    const engineOpt = engineOptions[0]; // デフォルトのエンジンオプションを使用
    const enginePool = useEnginePool({
        createClient:
            engineOpt?.createClient ??
            (() => {
                throw new Error("No engine available");
            }),
        workerCount: resolveWorkerCount(analysisSettings.parallelWorkers),
        onProgress: (progress) => {
            setBatchAnalysis({
                isRunning: true,
                currentIndex: progress.completed,
                totalCount: progress.total,
                targetPlies: [], // 進捗表示用には不要
                inProgress: progress.inProgress,
            });
        },
        onResult: (ply, event, nodeId) => {
            // nodeIdがある場合は分岐解析の結果
            if (nodeId) {
                recordEvalByNodeId(nodeId, event);
            } else {
                recordEvalByPly(ply, event);
            }
        },
        onComplete: () => {
            setBatchAnalysis(null);
        },
        onError: (ply, error) => {
            console.error(`解析エラー (ply=${ply}):`, error);
        },
    });

    // キーボード・ホイールナビゲーション用のgoForward（分岐対応）
    const handleKeyboardForward = useCallback(() => {
        navigation.goForward(selectedBranchNodeId ?? undefined);
    }, [navigation, selectedBranchNodeId]);

    // 盤面反転のハンドラ（メモ化）
    const handleFlipBoard = useCallback(() => {
        setFlipBoard((prev) => !prev);
    }, []);

    // キーボード・ホイールナビゲーション（対局中は無効）
    // selectedBranchNodeIdがある場合は、分岐に沿って進む
    useKifuKeyboardNavigation({
        onForward: handleKeyboardForward,
        onBack: navigation.goBack,
        onToStart: navigation.goToStart,
        onToEnd: navigation.goToEnd,
        disabled: isMatchRunning,
        containerRef: boardSectionRef,
        enableWheelNavigation: displaySettings.enableWheelNavigation,
    });

    // エンジンからの手を受け取って適用するコールバック
    const handleMoveFromEngine = useCallback(
        (move: string) => {
            if (matchEndedRef.current) return;
            const result = applyMoveWithState(positionRef.current, move, {
                validateTurn: false,
            });
            if (!result.ok) {
                logEngineError(
                    `engine move rejected (${move || "empty"}): ${result.error ?? "unknown"}`,
                );
                return;
            }
            // 消費時間を計算
            const elapsedMs = Date.now() - turnStartTimeRef.current;
            // 棋譜ナビゲーションに手を追加（局面更新はonPositionChangeで自動実行）
            navigation.addMove(move, result.next, { elapsedMs });
            movesRef.current = [...movesRef.current, move];
            setLastMove(result.lastMove);
            setSelection(null);
            setMessage(null);
            legalCache.clear();
            // ターン開始時刻をリセット
            turnStartTimeRef.current = Date.now();
            updateClocksForNextTurn(result.next.turn);
        },
        [legalCache, logEngineError, navigation, updateClocksForNextTurn],
    );
    handleMoveFromEngineRef.current = handleMoveFromEngine;

    // パス手を処理するコールバック
    // 人間・エンジン両方のパス手で使用される
    const handlePassMove = useCallback(async () => {
        if (matchEndedRef.current) return;
        if (!passRightsSettings?.enabled) return;

        // 合法手をチェック（王手中はパスが合法手に含まれない）
        // エンジン側の can_pass() は王手中のパスを禁止しており、
        // パスが合法でない場合にloadPositionするとパニックするため、事前にチェック
        try {
            const passRightsOption = buildPassRightsOptionForLegalMoves(
                passRightsSettings,
                movesRef.current,
            );
            const resolver = fetchLegalMoves
                ? () => fetchLegalMoves(startSfen, movesRef.current, passRightsOption)
                : () =>
                      getPositionService().getLegalMoves(
                          startSfen,
                          movesRef.current,
                          passRightsOption,
                      );
            const ply = movesRef.current.length;
            const legal = await legalCache.getOrResolve(ply, resolver);
            if (!legal || !legal.has("pass")) {
                setMessage({ text: "王手されているためパスできません", type: "error" });
                return;
            }
        } catch (error) {
            setMessage({ text: `合法手の取得に失敗しました: ${String(error)}`, type: "error" });
            return;
        }

        // "pass" を applyMoveWithState で適用
        // validateTurn: false の理由:
        // - 人間のパスはUI側で手番チェック済み（sides[position.turn].role === "human"）
        // - エンジンのパスも受け付けるため、ここでは手番検証をスキップ
        const result = applyMoveWithState(positionRef.current, "pass", {
            validateTurn: false,
        });

        if (!result.ok) {
            setMessage({ text: `パスに失敗しました: ${result.error}`, type: "error" });
            return;
        }

        // 消費時間を計算
        const elapsedMs = Date.now() - turnStartTimeRef.current;
        // 棋譜ナビゲーションに手を追加（局面更新はonPositionChangeで自動実行）
        navigation.addMove("pass", result.next, { elapsedMs });
        movesRef.current = [...movesRef.current, "pass"];
        setLastMove(result.lastMove);
        setSelection(null);
        setMessage(null);
        legalCache.clear();

        // ターン開始時刻をリセット
        turnStartTimeRef.current = Date.now();
        updateClocksForNextTurn(result.next.turn);
    }, [
        fetchLegalMoves,
        legalCache,
        navigation,
        passRightsSettings,
        startSfen,
        updateClocksForNextTurn,
    ]);

    useEffect(() => {
        let cancelled = false;
        const service = getPositionService();

        const init = async () => {
            try {
                const pos = await service.getInitialBoard();
                if (cancelled) return;
                setPosition(pos);
                positionRef.current = pos;
                setInitialBoard(cloneBoard(pos.board));
                setBasePosition(clonePositionState(pos));
                let sfen = "startpos";
                try {
                    sfen = await service.boardToSfen(pos);
                    if (!cancelled) {
                        setStartSfen(sfen);
                    }
                } catch (error) {
                    if (!cancelled) {
                        setMessage({
                            text: `局面のSFEN変換に失敗しました: ${String(error)}`,
                            type: "error",
                        });
                    }
                }
                // 棋譜ナビゲーションを正しい初期局面でリセット
                if (!cancelled) {
                    navigationResetRef.current(pos, sfen);
                    setPositionReady(true);
                }
            } catch (error) {
                if (!cancelled) {
                    setMessage({
                        text: `初期局面の取得に失敗しました: ${String(error)}`,
                        type: "error",
                    });
                }
            }
        };

        void init();
        return () => {
            cancelled = true;
        };
    }, []);

    const grid = useMemo(() => {
        const g = boardToGrid(position.board);
        return flipBoard ? [...g].reverse().map((row) => [...row].reverse()) : g;
    }, [position.board, flipBoard]);

    const refreshStartSfen = useCallback(async (pos: PositionState): Promise<string> => {
        try {
            const sfen = await getPositionService().boardToSfen(pos);
            setStartSfen(sfen);
            return sfen;
        } catch (error) {
            setMessage({ text: `局面のSFEN変換に失敗しました: ${String(error)}`, type: "error" });
            throw error;
        }
    }, []);

    const pauseAutoPlay = async () => {
        setIsMatchRunning(false);
        setIsPaused(true); // 一時停止モードに（棋譜を保持）
        stopTicking();
        await stopAllEngines();
    };

    /** 一時停止中から編集モードに移行 */
    const enterEditModeFromPaused = () => {
        setIsPaused(false);
        setIsEditMode(true);
    };

    const resumeAutoPlay = async () => {
        matchEndedRef.current = false;
        if (!positionReady) return;

        // 一時停止からの再開：棋譜を保持したまま再開
        if (isPaused) {
            setIsPaused(false);
            setIsMatchRunning(true);
            turnStartTimeRef.current = Date.now();
            startTicking(position.turn);
            return;
        }

        // 編集モードからの再開：棋譜をリセットして新しい対局を開始
        if (isEditMode) {
            await finalizeEditedPosition();
            // 対局開始時に編集モードを終了
            setIsEditMode(false);
        }

        // パス権が有効な場合、対局開始時に初期化
        if (passRightsSettings?.enabled && !positionRef.current.passRights) {
            const updatedPosition = {
                ...positionRef.current,
                passRights: {
                    sente: passRightsSettings.initialCount,
                    gote: passRightsSettings.initialCount,
                },
            };
            setPosition(updatedPosition);
            positionRef.current = updatedPosition;
        }

        // 盤面セクションにスクロール
        setTimeout(() => {
            boardSectionRef.current?.scrollIntoView({
                behavior: "smooth",
                block: "start",
            });
        }, 100);

        // エンジン管理は useEngineManager フックが自動的に処理する
        setIsMatchRunning(true);
        // ターン開始時刻をリセット
        turnStartTimeRef.current = Date.now();
        startTicking(position.turn);
    };

    /** 検討モードを開始 */
    const handleStartReview = async () => {
        if (!positionReady) return;
        if (isEditMode) {
            await finalizeEditedPosition();
            setIsEditMode(false);
        }
        // isMatchRunningはfalseのままでisReviewModeになる
    };

    /** 現在のゲームモードを計算 */
    const gameMode: GameMode = isEditMode
        ? "editing"
        : isMatchRunning
          ? "playing"
          : isPaused
            ? "paused"
            : "reviewing";

    const finalizeEditedPosition = async () => {
        if (isMatchRunning) return;
        const current = positionRef.current;
        setBasePosition(clonePositionState(current));
        setInitialBoard(cloneBoard(current.board));
        // SFENを取得して棋譜ツリーをリセット（編集した持ち駒情報を反映）
        try {
            const newSfen = await refreshStartSfen(current);
            navigation.reset(current, newSfen);
            movesRef.current = [];
            legalCache.clear();
            setIsEditMode(false);
            setMessage({
                text: "局面を確定しました。対局開始でこの局面から進行します。",
                type: "success",
            });
        } catch {
            setMessage({ text: "局面の確定に失敗しました。", type: "error" });
        }
    };

    /** 検討モードから編集モードに戻る */
    const handleEnterEditMode = useCallback(async () => {
        if (isMatchRunning) return;
        const current = positionRef.current;
        // 現在局面を編集開始局面として設定
        setBasePosition(clonePositionState(current));
        setInitialBoard(cloneBoard(current.board));
        // 先にSFENを取得してから棋譜ナビゲーションをリセット
        try {
            const newSfen = await refreshStartSfen(current);
            navigation.reset(current, newSfen);
            movesRef.current = [];
            setLastMove(undefined);
            setSelection(null);
            setMessage(null);
            setLastAddedBranchInfo(null);
            legalCache.clear();
            // 編集モードに移行
            setIsEditMode(true);
        } catch {
            setMessage({ text: "編集モードへの移行に失敗しました。", type: "error" });
        }
    }, [isMatchRunning, navigation, legalCache, refreshStartSfen]);

    const applyMoveCommon = useCallback(
        (nextPosition: PositionState, mv: string, last?: LastMove) => {
            // 消費時間を計算
            const elapsedMs = Date.now() - turnStartTimeRef.current;
            // 棋譜ナビゲーションに手を追加（局面更新はonPositionChangeで自動実行）
            navigation.addMove(mv, nextPosition, { elapsedMs });
            movesRef.current = [...movesRef.current, mv];
            setLastMove(last);
            setSelection(null);
            setMessage(null);
            legalCache.clear();
            // ターン開始時刻をリセット
            turnStartTimeRef.current = Date.now();
            updateClocksForNextTurn(nextPosition.turn);
        },
        [legalCache, navigation, updateClocksForNextTurn],
    );

    /** 検討モードで手を適用（分岐作成、時計更新なし） */
    const applyMoveForReview = useCallback(
        (nextPosition: PositionState, mv: string, last?: LastMove) => {
            // 現在のノードの子を確認して、分岐が作成されるか判定
            const tree = navigation.tree;
            const currentNode = tree ? tree.nodes.get(tree.currentNodeId) : null;

            const existingChild = currentNode?.children.find((childId: string) => {
                const child = tree?.nodes.get(childId);
                return child?.usiMove === mv;
            });
            const willCreateBranch = !existingChild && (currentNode?.children.length ?? 0) > 0;

            // 棋譜ナビゲーションに手を追加
            navigation.addMove(mv, nextPosition);
            movesRef.current = [...movesRef.current, mv];
            setLastMove(last);
            setSelection(null);
            setMessage(null);
            legalCache.clear();

            // 分岐が作成された場合は記録（ネスト分岐も含む）
            if (willCreateBranch && currentNode) {
                // 分岐点のply（currentNode）と最初の手（mv）を記録
                setLastAddedBranchInfo({ ply: currentNode.ply, firstMove: mv });
            }
        },
        [legalCache, navigation],
    );

    /** 平手初期局面にリセット */
    const handleResetToStartpos = useCallback(async () => {
        matchEndedRef.current = false;
        setGameResult(null);
        setShowResultDialog(false);
        await stopAllEngines();

        const service = getPositionService();
        try {
            const pos = await service.getInitialBoard();
            const next = clonePositionState(pos);
            setPosition(next);
            positionRef.current = next;
            setInitialBoard(cloneBoard(next.board));
            setBasePosition(clonePositionState(next));
            setStartSfen("startpos");
            setPositionReady(true);

            navigation.reset(next, "startpos");
            movesRef.current = [];
            setLastMove(undefined);
            setSelection(null);
            setMessage(null);
            setLastAddedBranchInfo(null); // 分岐状態をクリア
            resetClocks(false);

            setIsMatchRunning(false);
            setIsEditMode(true);
            setEditFromSquare(null);
            setEditTool("place");
            setEditPromoted(false);
            setEditOwner("sente");
            setEditPieceType(null);
            legalCache.clear();
            turnStartTimeRef.current = Date.now();
        } catch (error) {
            setMessage({ text: `平手初期化に失敗しました: ${String(error)}`, type: "error" });
        }
    }, [navigation, resetClocks, stopAllEngines, legalCache.clear]);

    const getLegalSet = useCallback(async (): Promise<Set<string> | null> => {
        if (!positionReady) return null;
        const ply = movesRef.current.length;
        const passRightsOption = buildPassRightsOptionForLegalMoves(
            passRightsSettings,
            movesRef.current,
        );
        const resolver = async () => {
            if (fetchLegalMoves) {
                return fetchLegalMoves(startSfen, movesRef.current, passRightsOption);
            }
            return getPositionService().getLegalMoves(
                startSfen,
                movesRef.current,
                passRightsOption,
            );
        };
        return legalCache.getOrResolve(ply, resolver);
    }, [positionReady, fetchLegalMoves, startSfen, legalCache, passRightsSettings]);

    const applyEditedPosition = useCallback(
        async (nextPosition: PositionState) => {
            // バージョンをインクリメントして現在の操作IDを取得
            editVersionRef.current += 1;
            const currentVersion = editVersionRef.current;

            setPosition(nextPosition);
            positionRef.current = nextPosition;
            setInitialBoard(cloneBoard(nextPosition.board));

            // 先にSFENを取得してから棋譜ナビゲーションをリセット
            try {
                const newSfen = await refreshStartSfen(nextPosition);

                // 古い操作の結果は無視（より新しい編集が既に開始されている場合）
                if (editVersionRef.current !== currentVersion) {
                    return;
                }

                navigation.reset(nextPosition, newSfen);

                movesRef.current = [];
                setLastMove(undefined);
                setSelection(null);
                setMessage(null);
                setLastAddedBranchInfo(null); // 分岐状態をクリア
                setEditFromSquare(null);

                legalCache.clear();
                stopTicking();
                matchEndedRef.current = false;
                setIsMatchRunning(false);
            } catch {
                // 古い操作のエラーは無視
                if (editVersionRef.current !== currentVersion) {
                    return;
                }
                setMessage({ text: "局面の適用に失敗しました。", type: "error" });
            }
        },
        [navigation, legalCache, stopTicking, refreshStartSfen],
    );

    const setPiecePromotion = useCallback(
        (square: Square, promote: boolean) => {
            if (!isEditMode) return;
            const current = positionRef.current;
            const piece = current.board[square];
            if (!piece) return;
            if (!isPromotable(piece.type)) {
                setMessage({ text: `${PIECE_LABELS[piece.type]}は成れません。`, type: "error" });
                return;
            }

            const nextBoard = cloneBoard(current.board);
            nextBoard[square] = promote
                ? { ...piece, promoted: true }
                : { ...piece, promoted: undefined };
            applyEditedPosition({ ...current, board: nextBoard });
        },
        [applyEditedPosition, isEditMode],
    );

    // DnD ドロップハンドラ
    const handleDndDrop = useCallback(
        (result: DropResult) => {
            if (!isEditMode) return;

            const applied = applyDropResult(result, positionRef.current);
            if (!applied.ok) {
                setMessage({ text: applied.error ?? "ドロップに失敗しました", type: "error" });
                return;
            }

            applyEditedPosition(applied.next);
        },
        [isEditMode, applyEditedPosition],
    );

    // DnD コントローラー
    const dndController = usePieceDnd({
        onDrop: handleDndDrop,
        disabled: !isEditMode,
    });

    // DnD ドラッグ開始ハンドラ（盤上の駒）
    // 注: isEditMode チェックは usePieceDnd の disabled オプションと
    //     JSX での条件付き props 渡しで行うため、ここでは不要
    const handlePiecePointerDown = useCallback(
        (
            square: string,
            piece: { owner: "sente" | "gote"; type: string; promoted?: boolean },
            e: React.PointerEvent,
        ) => {
            const origin = { type: "board" as const, square: square as Square };
            const payload = {
                owner: piece.owner as Player,
                pieceType: piece.type as PieceType,
                isPromoted: piece.promoted ?? false,
            };

            dndController.startDrag(origin, payload, e);
        },
        [dndController],
    );

    // DnD ドラッグ開始ハンドラ（持ち駒）
    const handleHandPiecePointerDown = useCallback(
        (owner: Player, pieceType: PieceType, e: React.PointerEvent) => {
            // 持ち駒が0個の場合はストック扱い（編集モード時、無限供給）
            const count = position?.hands[owner][pieceType] ?? 0;
            const origin =
                count > 0
                    ? { type: "hand" as const, owner, pieceType }
                    : { type: "stock" as const, owner, pieceType };
            const payload = {
                owner,
                pieceType,
                isPromoted: false,
            };

            dndController.startDrag(origin, payload, e);
        },
        [dndController, position],
    );

    const handlePieceTogglePromote = useCallback(
        (
            square: string,
            piece: { owner: "sente" | "gote"; type: string; promoted?: boolean },
            _event: React.MouseEvent<HTMLButtonElement>,
        ) => {
            if (!isEditMode) return;
            const sq = square as Square;
            setPiecePromotion(sq, !piece.promoted);
        },
        [isEditMode, setPiecePromotion],
    );

    // 持ち駒増加ハンドラ（編集モード用）
    const handleIncrementHand = useCallback(
        (owner: Player, pieceType: PieceType) => {
            if (isMatchRunning || !position) return;
            const counts = countPieces(position);
            const currentCount = counts[owner][pieceType];
            if (currentCount >= PIECE_CAP[pieceType]) return;

            const nextHands = addToHand(cloneHandsState(position.hands), owner, pieceType);
            const nextPosition = {
                ...position,
                hands: nextHands,
            };
            setPosition(nextPosition);
            positionRef.current = nextPosition;
        },
        [isMatchRunning, position],
    );

    // 持ち駒減少ハンドラ（編集モード用）
    const handleDecrementHand = useCallback(
        (owner: Player, pieceType: PieceType) => {
            if (isMatchRunning || !position) return;
            const count = position.hands[owner][pieceType] ?? 0;
            if (count <= 0) return;

            const nextHands = consumeFromHand(cloneHandsState(position.hands), owner, pieceType);
            if (nextHands) {
                const nextPosition = {
                    ...position,
                    hands: nextHands,
                };
                setPosition(nextPosition);
                positionRef.current = nextPosition;
            }
        },
        [isMatchRunning, position],
    );

    const placePieceAt = useCallback(
        (square: Square, piece: Piece | null, options?: { fromSquare?: Square }): boolean => {
            const current = positionRef.current;
            const nextBoard = cloneBoard(current.board);
            let workingHands = cloneHandsState(current.hands);

            if (options?.fromSquare) {
                nextBoard[options.fromSquare] = null;
            }

            const existing = nextBoard[square];
            if (existing) {
                const base = existing.type;
                workingHands = addToHand(workingHands, existing.owner, base);
            }

            if (!piece) {
                nextBoard[square] = null;
                const nextPosition: PositionState = {
                    ...current,
                    board: nextBoard,
                    hands: workingHands,
                };
                applyEditedPosition(nextPosition);
                return true;
            }

            const baseType = piece.type;
            const consumedHands = consumeFromHand(workingHands, piece.owner, baseType);
            const handsForPlacement = consumedHands ?? workingHands;
            const countsBefore = countPieces({
                ...current,
                board: nextBoard,
                hands: handsForPlacement,
            });
            const nextCount = countsBefore[piece.owner][baseType] + 1;
            if (nextCount > PIECE_CAP[baseType]) {
                setMessage({
                    text: `${piece.owner === "sente" ? "先手" : "後手"}の${PIECE_LABELS[baseType]}は最大${PIECE_CAP[baseType]}枚までです`,
                    type: "warning",
                });
                return false;
            }
            if (piece.type === "K" && countsBefore[piece.owner][baseType] >= PIECE_CAP.K) {
                setMessage({ text: "玉はそれぞれ1枚まで配置できます。", type: "warning" });
                return false;
            }

            nextBoard[square] = piece.promoted ? { ...piece, promoted: true } : { ...piece };
            const finalHands = consumedHands ?? workingHands;
            const nextPosition: PositionState = {
                ...current,
                board: nextBoard,
                hands: finalHands,
            };
            applyEditedPosition(nextPosition);
            return true;
        },
        [applyEditedPosition],
    );

    const handleSquareSelect = useCallback(
        async (square: string, shiftKey?: boolean) => {
            setMessage(null);
            if (isEditMode) {
                if (!positionReady) {
                    return;
                }
                const sq = square as Square;

                // 移動元が選択されている場合：移動先として処理
                if (editFromSquare) {
                    const from = editFromSquare;
                    if (from === sq) {
                        // 同じマスをクリック：選択解除
                        setEditFromSquare(null);
                        return;
                    }
                    const moving = position.board[from];
                    if (!moving) {
                        setEditFromSquare(null);
                        return;
                    }
                    const ok = placePieceAt(sq, moving, { fromSquare: from });
                    if (ok) {
                        setEditFromSquare(null);
                    }
                    return;
                }

                // 削除モード：駒を削除
                if (editTool === "erase") {
                    placePieceAt(sq, null);
                    return;
                }

                // 駒ボタンが選択されている場合：配置
                if (editPieceType) {
                    const pieceToPlace: Piece = {
                        owner: editOwner,
                        type: editPieceType,
                        promoted: editPromoted || undefined,
                    };
                    placePieceAt(sq, pieceToPlace);
                    return;
                }

                // 駒ボタン未選択：盤上の駒をクリックで移動元として選択
                const current = position.board[sq];
                if (current) {
                    setEditFromSquare(sq);
                    return;
                }

                // 空マスをクリックした場合は何もしない
                return;
            }

            // ========== 検討モード ==========
            // 自由に棋譜を閲覧し、任意の局面から分岐を作成できる
            if (isReviewMode) {
                if (!positionReady) {
                    return;
                }

                // 成り選択中の場合：キャンセル
                if (promotionSelection) {
                    setPromotionSelection(null);
                    setSelection(null);
                    return;
                }

                const sq = square as Square;

                // 駒を選択
                if (!selection) {
                    const piece = position.board[sq];
                    // 検討モードでは現在の手番の駒のみ動かせる
                    if (piece && piece.owner === position.turn) {
                        setSelection({ kind: "square", square: sq });
                    }
                    return;
                }

                // 持ち駒を打つ
                if (selection.kind === "hand") {
                    const moveStr = `${selection.piece}*${square}`;
                    const legal = await getLegalSet();
                    if (legal && !legal.has(moveStr)) {
                        setMessage({ text: "合法手ではありません", type: "error" });
                        return;
                    }
                    const result = applyMoveWithState(position, moveStr, { validateTurn: false });
                    if (!result.ok) {
                        setMessage({
                            text: result.error ?? "持ち駒を打てませんでした",
                            type: "error",
                        });
                        return;
                    }
                    applyMoveForReview(result.next, moveStr, result.lastMove);
                    return;
                }

                // 盤上の駒を移動
                if (selection.kind === "square") {
                    if (selection.square === square) {
                        setSelection(null);
                        return;
                    }

                    const legal = await getLegalSet();
                    if (!legal) return;

                    const from = selection.square;
                    const to = square;
                    const piece = position.board[from as Square];

                    const promotion = determinePromotion(legal, from, to);

                    if (promotion === "none") {
                        const moveStr = `${from}${to}`;
                        if (!legal.has(moveStr)) {
                            setMessage({ text: "合法手ではありません", type: "error" });
                            return;
                        }
                        const result = applyMoveWithState(position, moveStr, {
                            validateTurn: false,
                        });
                        if (!result.ok) {
                            setMessage({
                                text: result.error ?? "指し手を適用できませんでした",
                                type: "error",
                            });
                            return;
                        }
                        applyMoveForReview(result.next, moveStr, result.lastMove);
                        return;
                    }

                    if (promotion === "forced") {
                        const moveStr = `${from}${to}+`;
                        const result = applyMoveWithState(position, moveStr, {
                            validateTurn: false,
                        });
                        if (!result.ok) {
                            setMessage({
                                text: result.error ?? "指し手を適用できませんでした",
                                type: "error",
                            });
                            return;
                        }
                        applyMoveForReview(result.next, moveStr, result.lastMove);
                        return;
                    }

                    // 任意成り
                    if (shiftKey) {
                        const moveStr = `${from}${to}+`;
                        const result = applyMoveWithState(position, moveStr, {
                            validateTurn: false,
                        });
                        if (!result.ok) {
                            setMessage({
                                text: result.error ?? "指し手を適用できませんでした",
                                type: "error",
                            });
                            return;
                        }
                        applyMoveForReview(result.next, moveStr, result.lastMove);
                        return;
                    }

                    if (!piece) {
                        setMessage({ text: "駒が見つかりません", type: "error" });
                        return;
                    }
                    setPromotionSelection({ from: from as Square, to: to as Square, piece });
                    return;
                }
                return;
            }

            // ========== 対局モード ==========
            // 一時停止中は入力をブロック
            if (isPaused) {
                return;
            }
            if (!positionReady) {
                return;
            }
            if (isEngineTurn(position.turn)) {
                return;
            }

            // 成り選択中の場合：成り/不成を選択
            if (promotionSelection) {
                // 成り選択UIの外をクリック → キャンセル
                setPromotionSelection(null);
                setSelection(null);
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

                const legal = await getLegalSet();
                if (!legal) return;

                const from = selection.square;
                const to = square;
                const piece = position.board[from as Square];

                // 成り判定を実行
                const promotion = determinePromotion(legal, from, to);

                // 【ケース1】成れない場合 → 基本移動を試行
                if (promotion === "none") {
                    const moveStr = `${from}${to}`;
                    if (!legal.has(moveStr)) {
                        setMessage({ text: "合法手ではありません", type: "error" });
                        return;
                    }
                    const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                    if (!result.ok) {
                        setMessage({
                            text: result.error ?? "指し手を適用できませんでした",
                            type: "error",
                        });
                        return;
                    }
                    applyMoveCommon(result.next, moveStr, result.lastMove);
                    return;
                }

                // 【ケース2】強制成り → 自動的に成って移動（ダイアログなし）
                if (promotion === "forced") {
                    const moveStr = `${from}${to}+`;
                    const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                    if (!result.ok) {
                        setMessage({
                            text: result.error ?? "指し手を適用できませんでした",
                            type: "error",
                        });
                        return;
                    }
                    applyMoveCommon(result.next, moveStr, result.lastMove);
                    return;
                }

                // 【ケース3】任意成り（promotion === 'optional'）
                // Shift+クリック：即座に成って移動
                if (shiftKey) {
                    const moveStr = `${from}${to}+`;
                    const result = applyMoveWithState(position, moveStr, { validateTurn: true });
                    if (!result.ok) {
                        setMessage({
                            text: result.error ?? "指し手を適用できませんでした",
                            type: "error",
                        });
                        return;
                    }
                    applyMoveCommon(result.next, moveStr, result.lastMove);
                    return;
                }

                // 通常クリック：成り選択ダイアログを表示
                if (!piece) {
                    setMessage({ text: "駒が見つかりません", type: "error" });
                    return;
                }
                setPromotionSelection({ from: from as Square, to: to as Square, piece });
                return;
            }

            // 持ち駒を打つ
            const moveStr = `${selection.piece}*${square}`;
            const legal = await getLegalSet();
            if (legal && !legal.has(moveStr)) {
                setMessage({ text: "合法手ではありません", type: "error" });
                return;
            }
            const result = applyMoveWithState(position, moveStr, { validateTurn: true });
            if (!result.ok) {
                setMessage({ text: result.error ?? "持ち駒を打てませんでした", type: "error" });
                return;
            }
            applyMoveCommon(result.next, moveStr, result.lastMove);
        },
        [
            isEditMode,
            positionReady,
            editFromSquare,
            position,
            editTool,
            editPieceType,
            editOwner,
            editPromoted,
            isReviewMode,
            isPaused,
            promotionSelection,
            selection,
            isEngineTurn,
            applyMoveCommon,
            applyMoveForReview,
            getLegalSet,
            placePieceAt,
        ],
    );

    const handlePromotionChoice = useCallback(
        (promote: boolean) => {
            if (!promotionSelection) return;
            const { from, to } = promotionSelection;
            const moveStr = `${from}${to}${promote ? "+" : ""}`;
            // 検討モードでは手番チェックをスキップ
            const result = applyMoveWithState(position, moveStr, { validateTurn: !isReviewMode });
            if (!result.ok) {
                setMessage({ text: result.error ?? "指し手を適用できませんでした", type: "error" });
                setPromotionSelection(null);
                setSelection(null);
                return;
            }
            if (isReviewMode) {
                applyMoveForReview(result.next, moveStr, result.lastMove);
            } else {
                applyMoveCommon(result.next, moveStr, result.lastMove);
            }
            setPromotionSelection(null);
        },
        [promotionSelection, position, isReviewMode, applyMoveForReview, applyMoveCommon],
    );

    const handleHandSelect = useCallback(
        (piece: PieceType) => {
            if (!positionReady) {
                return;
            }
            if (isEditMode) {
                return;
            }
            // 検討モードでは手番の持ち駒を選択可能
            if (!isReviewMode && isEngineTurn(position.turn)) {
                return;
            }
            setSelection({ kind: "hand", piece });
            setMessage(null);
        },
        [positionReady, isEditMode, isReviewMode, isEngineTurn, position.turn],
    );

    const loadMoves = useCallback(
        async (
            list: string[],
            moveData: KifMoveData[] | undefined,
            startPosition: PositionState,
            startSfenToLoad: string,
        ) => {
            const filtered = list.filter(Boolean);
            const service = getPositionService();
            // パス入り棋譜の場合はpassRightsを渡す
            const passRightsOption = buildPassRightsOptionForLegalMoves(
                passRightsSettings,
                filtered,
            );
            const result = await service.replayMovesStrict(
                startSfenToLoad,
                filtered,
                passRightsOption,
            );

            // 棋譜ナビゲーションをリセット
            navigation.reset(startPosition, startSfenToLoad);
            setLastAddedBranchInfo(null); // 分岐状態をクリア

            // 各手を順番に追加
            let currentPos = startPosition;
            for (let i = 0; i < result.applied.length; i++) {
                const move = result.applied[i];
                const data = moveData?.[i];
                const applyResult = applyMoveWithState(currentPos, move, {
                    validateTurn: false,
                });
                if (applyResult.ok) {
                    // 消費時間と評価値を渡す
                    // KIFインポートの評価値は既に先手視点なので normalized: true
                    navigation.addMove(move, applyResult.next, {
                        elapsedMs: data?.elapsedMs,
                        eval:
                            data?.evalCp !== undefined || data?.evalMate !== undefined
                                ? {
                                      scoreCp: data.evalCp,
                                      scoreMate: data.evalMate,
                                      depth: data.depth,
                                      normalized: true,
                                  }
                                : undefined,
                    });
                    currentPos = applyResult.next;
                }
            }

            movesRef.current = result.applied;
            setLastMove(deriveLastMove(result.applied.at(-1)));
            setSelection(null);
            setMessage(null);
            resetClocks(false);

            legalCache.clear();
            setPositionReady(true);

            if (result.error) {
                throw new Error(result.error);
            }
        },
        [navigation, resetClocks, legalCache, passRightsSettings],
    );

    // KIFコピー用コールバック
    const handleCopyKif = useCallback((): string => {
        return exportToKifString(kifMoves, boardHistory, {
            startTime: new Date(),
            senteName: sides.sente.role === "engine" ? "エンジン" : "人間",
            goteName: sides.gote.role === "engine" ? "エンジン" : "人間",
            includeEval: true, // 評価値もコメントとして出力
            startSfen,
        });
    }, [kifMoves, boardHistory, sides.sente.role, sides.gote.role, startSfen]);

    // 棋譜の手数選択コールバック（巻き戻し・リプレイ用）
    const handlePlySelect = useCallback(
        (ply: number) => {
            // 対局中は自動進行を一時停止し、編集モードに戻す
            if (isMatchRunning) {
                setIsMatchRunning(false);
                setIsEditMode(true);
                stopTicking();
                void stopAllEngines();
            }
            // 指定手数に移動（lastMoveはonPositionChangeで自動設定される）
            navigation.goToPly(ply);
        },
        [isMatchRunning, navigation, stopTicking, stopAllEngines],
    );

    // 特定の手数の局面を解析するコールバック（オンデマンド解析用）
    const handleAnalyzePly = useCallback(
        (ply: number) => {
            // ply手目の局面を解析するには、ply-1手までの指し手が必要
            // （ply 1 = 1手目を指した後の局面 = moves[0]まで適用した局面）
            const movesForPly = kifMoves.slice(0, ply).map((m) => m.usiMove);

            setAnalyzingState({ type: "by-ply", ply });
            void analyzePosition({
                sfen: startSfen,
                moves: movesForPly,
                ply,
                timeMs: 3000, // 3秒間解析
                depth: 20, // 最大深さ20
            });
        },
        [kifMoves, analyzePosition, startSfen],
    );

    // 分岐内のノードを解析するコールバック
    const handleAnalyzeNode = useCallback(
        async (nodeId: string) => {
            const tree = navigation.tree;
            if (!tree) {
                setMessage({ text: "棋譜ツリーが初期化されていません", type: "error" });
                return;
            }

            const node = tree.nodes.get(nodeId);
            if (!node) {
                setMessage({ text: "指定されたノードが見つかりません", type: "error" });
                return;
            }

            try {
                // ルートからこのノードまでのパスを取得
                const path = getPathToNode(tree, nodeId);
                // 各ノードのusiMoveを収集（ルートは除く）
                const movesForNode: string[] = [];
                for (const id of path) {
                    const n = tree.nodes.get(id);
                    if (n?.usiMove) {
                        movesForNode.push(n.usiMove);
                    }
                }

                // 分岐解析用に状態を設定
                setAnalyzingState({ type: "by-node-id", nodeId, ply: node.ply });
                await analyzePosition({
                    sfen: startSfen,
                    moves: movesForNode,
                    ply: node.ply,
                    timeMs: 3000,
                    depth: 20,
                });
            } catch (error) {
                setMessage({
                    text: `解析エラー: ${error instanceof Error ? error.message : String(error)}`,
                    type: "error",
                });
                setAnalyzingState(ANALYZING_STATE_NONE);
            }
        },
        [navigation.tree, analyzePosition, startSfen],
    );

    // 単発解析完了時の処理
    useEffect(() => {
        if (!isAnalyzing && analyzingState.type !== "none") {
            setAnalyzingState(ANALYZING_STATE_NONE);
        }
    }, [isAnalyzing, analyzingState.type]);

    // 一括解析を開始（並列処理）- 本譜のみ
    const handleStartBatchAnalysis = useCallback(() => {
        // PVがない手を抽出
        const targetPlies = kifMoves.filter((m) => !m.pv || m.pv.length === 0).map((m) => m.ply);

        if (targetPlies.length === 0) {
            return; // 解析対象がない
        }

        // ジョブを生成
        const jobs: AnalysisJob[] = targetPlies.map((ply) => ({
            ply,
            sfen: startSfen,
            moves: kifMoves.slice(0, ply).map((m) => m.usiMove),
            timeMs: analysisSettings.batchAnalysisTimeMs,
            depth: analysisSettings.batchAnalysisDepth,
        }));

        // 並列一括解析を開始
        enginePool.start(jobs);
    }, [kifMoves, startSfen, analysisSettings, enginePool]);

    // ツリー全体（分岐含む）の一括解析を開始
    const handleStartTreeBatchAnalysis = useCallback(
        (options?: { mainLineOnly?: boolean }) => {
            const tree = navigation.tree;
            if (!tree) return;

            // ツリーから解析ジョブを収集
            const treeJobs = collectTreeAnalysisJobs(tree, {
                onlyWithoutEval: true,
                mainLineOnly: options?.mainLineOnly ?? false,
            });

            if (treeJobs.length === 0) {
                setMessage({ text: "解析対象の手がありません", type: "warning" });
                setTimeout(() => setMessage(null), 3000);
                return;
            }

            // AnalysisJob形式に変換
            const jobs: AnalysisJob[] = treeJobs.map((job) => ({
                ply: job.ply,
                sfen: startSfen,
                moves: job.moves,
                timeMs: analysisSettings.batchAnalysisTimeMs,
                depth: analysisSettings.batchAnalysisDepth,
                nodeId: job.nodeId, // 分岐解析用にnodeIdを保持
            }));

            // 並列一括解析を開始
            enginePool.start(jobs);
        },
        [navigation.tree, startSfen, analysisSettings, enginePool],
    );

    // 特定の分岐を一括解析
    const handleAnalyzeBranch = useCallback(
        (branchNodeId: string) => {
            const tree = navigation.tree;
            if (!tree) return;

            // 分岐から解析ジョブを収集
            const branchJobs = collectBranchAnalysisJobs(tree, branchNodeId, {
                onlyWithoutEval: true,
            });

            if (branchJobs.length === 0) {
                return;
            }

            // AnalysisJob形式に変換
            const jobs: AnalysisJob[] = branchJobs.map((job) => ({
                ply: job.ply,
                sfen: startSfen,
                moves: job.moves,
                timeMs: analysisSettings.batchAnalysisTimeMs,
                depth: analysisSettings.batchAnalysisDepth,
                nodeId: job.nodeId,
            }));

            // 並列一括解析を開始
            enginePool.start(jobs);
        },
        [navigation.tree, startSfen, analysisSettings, enginePool],
    );

    // 分岐作成時の自動解析
    useEffect(() => {
        if (!lastAddedBranchInfo || analysisSettings.autoAnalyzeMode === "off") {
            return;
        }

        const runAnalysis = () => {
            // ply + firstMove から分岐のnodeIdを見つける
            const branches = getAllBranches(navigation.tree);
            const branch = branches.find((b) => {
                if (b.ply !== lastAddedBranchInfo.ply) return false;
                const node = navigation.tree.nodes.get(b.nodeId);
                return node?.usiMove === lastAddedBranchInfo.firstMove;
            });
            if (branch) {
                handleAnalyzeBranch(branch.nodeId);
            }
        };

        if (analysisSettings.autoAnalyzeMode === "immediate") {
            // 即時モード: すぐに解析開始
            runAnalysis();
        } else {
            // delayedモード: 3秒後に解析開始（操作が続けばリセット）
            const timerId = setTimeout(runAnalysis, 3000);
            return () => clearTimeout(timerId);
        }
    }, [
        lastAddedBranchInfo,
        analysisSettings.autoAnalyzeMode,
        handleAnalyzeBranch,
        navigation.tree,
    ]);

    // 一括解析をキャンセル
    const handleCancelBatchAnalysis = useCallback(() => {
        void enginePool.cancel();
        setBatchAnalysis(null);
    }, [enginePool]);

    // PVを分岐として追加するコールバック（シグナル付き）
    const handleAddPvAsBranch = useCallback(
        (ply: number, pv: string[]) => {
            // 分岐が実際に追加された場合、ply+firstMoveを記録
            addPvAsBranch(ply, pv, (info) => {
                setLastAddedBranchInfo(info);
            });
        },
        [addPvAsBranch],
    );

    // PVプレビューを開くコールバック
    const handlePreviewPv = useCallback(
        (ply: number, pv: string[], evalCp?: number, evalMate?: number) => {
            // PVはply手目を指した後の局面から計算されている
            // positionHistory[ply-1] = ply手目を指した後の局面
            const startPos = positionHistory[ply - 1];
            if (!startPos) return;

            setPvPreview({
                open: true,
                ply,
                pv,
                startPosition: startPos,
                evalCp,
                evalMate,
            });
        },
        [positionHistory],
    );

    // 手の詳細を選択するコールバック（右パネル表示用）
    const handleMoveDetailSelect = useCallback(
        (move: KifMove | null, pos: PositionState | null) => {
            if (move && pos) {
                setSelectedMoveDetail({ move, position: pos });
            } else {
                setSelectedMoveDetail(null);
            }
        },
        [],
    );

    // SFENインポート（局面 + 指し手）
    // インポート後は自動的に検討モードに入る
    const importSfen = useCallback(
        async (sfen: string, movesToLoad: string[]) => {
            const service = getPositionService();
            try {
                // 新しい開始局面を設定
                const newPosition = await service.parseSfen(sfen);
                setBasePosition(newPosition);
                setStartSfen(sfen);
                setInitialBoard(newPosition.board);

                // 棋譜ナビゲーションをリセット
                navigation.reset(newPosition, sfen);
                setLastAddedBranchInfo(null); // 分岐状態をクリア

                // 指し手がある場合は適用
                if (movesToLoad.length > 0) {
                    let currentPos = newPosition;
                    const appliedMoves: string[] = [];
                    for (const move of movesToLoad) {
                        const applyResult = applyMoveWithState(currentPos, move, {
                            validateTurn: false,
                        });
                        if (applyResult.ok) {
                            navigation.addMove(move, applyResult.next);
                            currentPos = applyResult.next;
                            appliedMoves.push(move);
                        } else {
                            break;
                        }
                    }
                    movesRef.current = appliedMoves;
                    setLastMove(deriveLastMove(appliedMoves.at(-1)));
                } else {
                    movesRef.current = [];
                    setLastMove(undefined);
                }

                setSelection(null);
                resetClocks(false);
                legalCache.clear();
                setPositionReady(true);

                // インポート後は自動的に検討モードに入る
                setIsEditMode(false);
                setIsMatchRunning(false);
            } catch (error) {
                throw new Error(`SFENの適用に失敗しました: ${String(error)}`);
            }
        },
        [navigation, resetClocks, legalCache],
    );

    // KIFインポート（開始局面情報があれば使用）
    // インポート後は自動的に検討モードに入る
    const importKif = useCallback(
        async (movesToLoad: string[], moveData: KifMoveData[], startSfenFromKif?: string) => {
            const service = getPositionService();

            let startPosition: PositionState;
            let startSfenToLoad: string;

            if (startSfenFromKif?.trim()) {
                const parsed = parseSfen(startSfenFromKif);
                if (!parsed.sfen) {
                    throw new Error("開始局面のSFENが空です。");
                }
                startSfenToLoad = parsed.sfen;
                try {
                    startPosition = await service.parseSfen(startSfenToLoad);
                } catch (error) {
                    throw new Error(`開始局面の解析に失敗しました: ${String(error)}`);
                }
            } else {
                startSfenToLoad = "startpos";
                startPosition = await service.parseSfen(startSfenToLoad);
            }

            setBasePosition(startPosition);
            setStartSfen(startSfenToLoad);
            setInitialBoard(cloneBoard(startPosition.board));

            await loadMoves(movesToLoad, moveData, startPosition, startSfenToLoad);

            // KIFインポート後は自動的に検討モードに入る
            setIsEditMode(false);
            setIsMatchRunning(false);
        },
        [loadMoves],
    );

    const candidateNote = positionReady ? null : "局面を読み込み中です。";
    const isDraggingPiece = isEditMode && dndController.state.isDragging;

    // モバイル判定
    const isMobile = useIsMobile();

    const uiEngineOptions = useMemo(() => {
        // 内蔵エンジンの A/B スロットは UI に露出させず、単一の「内蔵エンジン」として扱う。
        const internal = engineOptions.find((opt) => opt.kind === "internal") ?? engineOptions[0];
        const externals = engineOptions.filter((opt) => opt.kind === "external");
        const list: EngineOption[] = [];
        if (internal) list.push({ ...internal, id: internal.id, label: "内蔵エンジン" });
        return [...list, ...externals];
    }, [engineOptions]);

    return (
        <TooltipProvider delayDuration={TOOLTIP_DELAY_DURATION_MS}>
            {/* DnD ゴースト */}
            <DragGhost
                ref={dndController.ghostRef as React.RefObject<HTMLDivElement>}
                dndState={dndController.state}
                ownerOrientation={flipBoard ? "gote" : "sente"}
            />

            {/* 勝敗表示ダイアログ */}
            <GameResultDialog
                result={gameResult}
                open={showResultDialog}
                onClose={() => setShowResultDialog(false)}
            />

            {/* PVプレビューダイアログ */}
            {pvPreview && (
                <PvPreviewDialog
                    open={pvPreview.open}
                    onClose={() => setPvPreview(null)}
                    pv={pvPreview.pv}
                    startPosition={pvPreview.startPosition}
                    ply={pvPreview.ply}
                    evalCp={pvPreview.evalCp}
                    evalMate={pvPreview.evalMate}
                    squareNotation={displaySettings.squareNotation}
                    showBoardLabels={displaySettings.showBoardLabels}
                />
            )}

            {/* 左上メニュー（画面固定、PC版のみ） */}
            {!isMobile && (
                <div className="fixed top-4 left-4 z-[100]">
                    <AppMenu
                        settings={displaySettings}
                        onSettingsChange={setDisplaySettings}
                        analysisSettings={analysisSettings}
                        onAnalysisSettingsChange={setAnalysisSettings}
                    />
                </div>
            )}

            {/* モバイル時はMobileLayout、PC時は3列レイアウト */}
            {isMobile ? (
                <MobileLayout
                    grid={grid}
                    position={position}
                    flipBoard={flipBoard}
                    lastMove={lastMove}
                    selection={selection}
                    promotionSelection={promotionSelection}
                    isEditMode={isEditMode}
                    isMatchRunning={isMatchRunning}
                    gameMode={gameMode}
                    editFromSquare={editFromSquare}
                    moves={moves}
                    candidateNote={candidateNote}
                    displaySettings={displaySettings}
                    onSquareSelect={handleSquareSelect}
                    onPromotionChoice={handlePromotionChoice}
                    onFlipBoard={handleFlipBoard}
                    onHandSelect={handleHandSelect}
                    onPiecePointerDown={isEditMode ? handlePiecePointerDown : undefined}
                    onPieceTogglePromote={isEditMode ? handlePieceTogglePromote : undefined}
                    onHandPiecePointerDown={isEditMode ? handleHandPiecePointerDown : undefined}
                    onIncrementHand={handleIncrementHand}
                    onDecrementHand={handleDecrementHand}
                    isReviewMode={isReviewMode}
                    getHandInfo={getHandInfo}
                    boardSectionRef={boardSectionRef}
                    isDraggingPiece={isDraggingPiece}
                    // 棋譜関連
                    kifMoves={kifMoves}
                    currentPly={navigation.state.currentPly}
                    totalPly={navigation.state.totalPly}
                    onPlySelect={handlePlySelect}
                    // ナビゲーション
                    onBack={navigation.goBack}
                    onForward={handleKeyboardForward}
                    onToStart={navigation.goToStart}
                    onToEnd={navigation.goToEnd}
                    // 評価値
                    evalHistory={evalHistory}
                    evalCp={evalHistory[navigation.state.currentPly]?.evalCp ?? undefined}
                    evalMate={evalHistory[navigation.state.currentPly]?.evalMate ?? undefined}
                    // 対局コントロール
                    onStop={pauseAutoPlay}
                    onStart={resumeAutoPlay}
                    onResetToStartpos={handleResetToStartpos}
                    onResign={handleResign}
                    onUndo={handleUndo}
                    canUndo={moves.length > 0}
                    onEnterEditMode={isPaused ? enterEditModeFromPaused : undefined}
                    // 対局設定
                    sides={sides}
                    onSidesChange={setSides}
                    timeSettings={timeSettings}
                    onTimeSettingsChange={setTimeSettings}
                    uiEngineOptions={uiEngineOptions}
                    settingsLocked={settingsLocked}
                    // パス権設定
                    passRightsSettings={passRightsSettings}
                    onPassRightsSettingsChange={handlePassRightsSettingsChange}
                    onPassMove={handlePassMove}
                    canPassMove={
                        isMatchRunning && sides[position.turn].role === "human" && canPass(position)
                    }
                    // クロック表示
                    clocks={clocks}
                    // 表示設定
                    displaySettingsFull={displaySettings}
                    onDisplaySettingsChange={setDisplaySettings}
                    // メッセージ
                    message={message}
                />
            ) : (
                <section className={matchLayoutClasses} style={matchLayoutCssVars}>
                    <div className="flex gap-4 items-start min-h-[calc(100dvh-1rem)]">
                        {/* 左列: 将棋盤（サイズ固定） */}
                        <div className="flex flex-col gap-2 items-center shrink-0 self-center">
                            <div
                                ref={boardSectionRef}
                                className="w-fit relative flex flex-col gap-2"
                            >
                                <div
                                    className={`flex flex-col gap-2 items-center ${isDraggingPiece ? "touch-none" : ""}`}
                                >
                                    {/* 時間管理（将棋盤の上） */}
                                    <ClockDisplay
                                        clocks={clocks}
                                        sides={sides}
                                        isRunning={isMatchRunning}
                                    />

                                    {/* 盤の上側の持ち駒（通常:後手、反転時:先手） */}
                                    {(() => {
                                        const info = getHandInfo("top");
                                        return (
                                            <div
                                                data-zone={`hand-${info.owner}`}
                                                className="w-full"
                                            >
                                                {/* ステータス行: [手数] [手番] [反転ボタン] */}
                                                <div className="flex items-center justify-end mb-1 gap-4">
                                                    {/* 手数表示 */}
                                                    <output
                                                        className={`${TEXT_CLASSES.moveCount} !m-0 whitespace-nowrap`}
                                                    >
                                                        {moves.length === 0
                                                            ? "開始局面"
                                                            : `${moves.length}手目`}
                                                    </output>

                                                    {/* 手番表示 */}
                                                    <output
                                                        className={`${TEXT_CLASSES.mutedSecondary} whitespace-nowrap`}
                                                    >
                                                        手番:{" "}
                                                        <span
                                                            className={`font-semibold text-[18px] ${
                                                                position.turn === "sente"
                                                                    ? "text-wafuu-shu"
                                                                    : "text-wafuu-ai"
                                                            }`}
                                                        >
                                                            {position.turn === "sente" ? "☗" : "☖"}
                                                        </span>
                                                    </output>

                                                    {/* 反転ボタン */}
                                                    <button
                                                        type="button"
                                                        onClick={() => setFlipBoard(!flipBoard)}
                                                        className={`flex items-center gap-1 px-2 py-1 rounded-md border border-[hsl(var(--wafuu-border))] cursor-pointer text-[13px] whitespace-nowrap ${
                                                            flipBoard
                                                                ? "bg-[hsl(var(--wafuu-kin)/0.2)]"
                                                                : "bg-card"
                                                        }`}
                                                        title="盤面を反転"
                                                    >
                                                        <span>🔄</span>
                                                        <span>反転</span>
                                                    </button>
                                                </div>

                                                {/* 持ち駒表示 */}
                                                <HandPiecesDisplay
                                                    owner={info.owner}
                                                    hand={info.hand}
                                                    selectedPiece={
                                                        selection?.kind === "hand"
                                                            ? selection.piece
                                                            : null
                                                    }
                                                    isActive={info.isActive}
                                                    onHandSelect={handleHandSelect}
                                                    onPiecePointerDown={
                                                        isEditMode
                                                            ? handleHandPiecePointerDown
                                                            : undefined
                                                    }
                                                    isEditMode={isEditMode && !isMatchRunning}
                                                    isMatchRunning={isMatchRunning}
                                                    onIncrement={(piece) =>
                                                        handleIncrementHand(info.owner, piece)
                                                    }
                                                    onDecrement={(piece) =>
                                                        handleDecrementHand(info.owner, piece)
                                                    }
                                                    flipBoard={flipBoard}
                                                />
                                            </div>
                                        );
                                    })()}

                                    {/* 盤面 */}
                                    <ShogiBoard
                                        grid={grid}
                                        selectedSquare={
                                            isEditMode && editFromSquare
                                                ? editFromSquare
                                                : selection?.kind === "square"
                                                  ? selection.square
                                                  : null
                                        }
                                        lastMove={
                                            displaySettings.highlightLastMove && lastMove
                                                ? {
                                                      from: lastMove.from ?? undefined,
                                                      to: lastMove.to,
                                                  }
                                                : undefined
                                        }
                                        promotionSquare={promotionSelection?.to ?? null}
                                        onSelect={(sq, shiftKey) => {
                                            void handleSquareSelect(sq, shiftKey);
                                        }}
                                        onPromotionChoice={handlePromotionChoice}
                                        flipBoard={flipBoard}
                                        onPiecePointerDown={
                                            isEditMode ? handlePiecePointerDown : undefined
                                        }
                                        onPieceTogglePromote={
                                            isEditMode ? handlePieceTogglePromote : undefined
                                        }
                                        isDraggable={isEditMode}
                                        squareNotation={displaySettings.squareNotation}
                                        showBoardLabels={displaySettings.showBoardLabels}
                                    />
                                    {candidateNote ? (
                                        <div className={TEXT_CLASSES.mutedSecondary}>
                                            {candidateNote}
                                        </div>
                                    ) : null}

                                    {/* 盤の下側の持ち駒（通常:先手、反転時:後手） */}
                                    {(() => {
                                        const info = getHandInfo("bottom");
                                        return (
                                            <PlayerHandSection
                                                owner={info.owner}
                                                hand={info.hand}
                                                selectedPiece={
                                                    selection?.kind === "hand"
                                                        ? selection.piece
                                                        : null
                                                }
                                                isActive={info.isActive}
                                                onHandSelect={handleHandSelect}
                                                onPiecePointerDown={
                                                    isEditMode
                                                        ? handleHandPiecePointerDown
                                                        : undefined
                                                }
                                                isEditMode={isEditMode && !isMatchRunning}
                                                isMatchRunning={isMatchRunning}
                                                onIncrement={(piece) =>
                                                    handleIncrementHand(info.owner, piece)
                                                }
                                                onDecrement={(piece) =>
                                                    handleDecrementHand(info.owner, piece)
                                                }
                                                flipBoard={flipBoard}
                                            />
                                        );
                                    })()}

                                    {/* パス権表示とパスボタン（initialCount > 0 の場合のみ表示） */}
                                    {passRightsSettings?.enabled &&
                                        passRightsSettings.initialCount > 0 &&
                                        position.passRights && (
                                            <div className="flex items-center justify-between w-full px-2 gap-2">
                                                <div className="flex items-center gap-4 text-xs text-muted-foreground">
                                                    <span className="flex items-center gap-1">
                                                        先手:
                                                        <PassRightsDisplay
                                                            remaining={position.passRights.sente}
                                                            max={passRightsSettings.initialCount}
                                                            isActive={position.turn === "sente"}
                                                        />
                                                    </span>
                                                    <span className="flex items-center gap-1">
                                                        後手:
                                                        <PassRightsDisplay
                                                            remaining={position.passRights.gote}
                                                            max={passRightsSettings.initialCount}
                                                            isActive={position.turn === "gote"}
                                                        />
                                                    </span>
                                                </div>
                                                {/* 人間の手番でパス可能な場合のみボタン表示 */}
                                                {isMatchRunning &&
                                                    sides[position.turn].role === "human" &&
                                                    canPass(position) && (
                                                        <PassButton
                                                            canPass={true}
                                                            onPass={handlePassMove}
                                                            remainingPassRights={
                                                                position.passRights[position.turn]
                                                            }
                                                            showConfirmDialog={true}
                                                        />
                                                    )}
                                            </div>
                                        )}

                                    {/* 対局コントロール（盤面の下） */}
                                    <MatchControls
                                        onResetToStartpos={handleResetToStartpos}
                                        onStop={pauseAutoPlay}
                                        onStart={resumeAutoPlay}
                                        onStartReview={handleStartReview}
                                        onEnterEditMode={
                                            isPaused ? enterEditModeFromPaused : handleEnterEditMode
                                        }
                                        onResign={handleResign}
                                        onUndo={handleUndo}
                                        canUndo={moves.length > 0}
                                        isMatchRunning={isMatchRunning}
                                        gameMode={gameMode}
                                        message={message}
                                        onOpenSettings={() => setIsSettingsModalOpen(true)}
                                    />
                                </div>
                            </div>
                        </div>

                        {/* 棋譜列 + 詳細ドロワー */}
                        <div className="flex flex-col gap-2 shrink-0 pt-16">
                            {/* 評価値グラフパネル（折りたたみ） */}
                            <EvalPanel
                                evalHistory={evalHistory}
                                currentPly={navigation.state.currentPly}
                                onPlySelect={handlePlySelect}
                                defaultOpen={false}
                            />

                            {/* 棋譜パネル + ドロワー（横並び） */}
                            <div className="relative flex items-start">
                                {/* 棋譜パネル（常時表示） */}
                                <KifuPanel
                                    kifMoves={kifMoves}
                                    currentPly={navigation.state.currentPly}
                                    showEval={displaySettings.showKifuEval}
                                    onShowEvalChange={(show) =>
                                        setDisplaySettings((prev) => ({
                                            ...prev,
                                            showKifuEval: show,
                                        }))
                                    }
                                    onPlySelect={handlePlySelect}
                                    onCopyKif={handleCopyKif}
                                    navigation={{
                                        currentPly: navigation.state.currentPly,
                                        totalPly: navigation.state.totalPly,
                                        onBack: navigation.goBack,
                                        onForward: () =>
                                            navigation.goForward(selectedBranchNodeId ?? undefined),
                                        onToStart: navigation.goToStart,
                                        onToEnd: navigation.goToEnd,
                                        isRewound: navigation.state.isRewound,
                                        canGoForward: navigation.state.canGoForward,
                                        branchInfo: navigation.state.hasBranches
                                            ? {
                                                  hasBranches: true,
                                                  currentIndex: navigation.state.currentBranchIndex,
                                                  count: navigation.state.branchCount,
                                                  onSwitch: navigation.switchBranch,
                                                  onPromoteToMain: navigation.promoteCurrentLine,
                                              }
                                            : undefined,
                                    }}
                                    navigationDisabled={isMatchRunning}
                                    branchMarkers={branchMarkers}
                                    positionHistory={positionHistory}
                                    onAddPvAsBranch={handleAddPvAsBranch}
                                    onPreviewPv={handlePreviewPv}
                                    lastAddedBranchInfo={lastAddedBranchInfo}
                                    onLastAddedBranchHandled={() => setLastAddedBranchInfo(null)}
                                    onSelectedBranchChange={setSelectedBranchNodeId}
                                    onAnalyzePly={handleAnalyzePly}
                                    isAnalyzing={isAnalyzing}
                                    analyzingPly={
                                        analyzingState.type !== "none"
                                            ? analyzingState.ply
                                            : undefined
                                    }
                                    batchAnalysis={
                                        batchAnalysis
                                            ? {
                                                  isRunning: batchAnalysis.isRunning,
                                                  currentIndex: batchAnalysis.currentIndex,
                                                  totalCount: batchAnalysis.totalCount,
                                                  inProgress: batchAnalysis.inProgress,
                                              }
                                            : undefined
                                    }
                                    onStartBatchAnalysis={handleStartBatchAnalysis}
                                    onCancelBatchAnalysis={handleCancelBatchAnalysis}
                                    analysisSettings={analysisSettings}
                                    onAnalysisSettingsChange={setAnalysisSettings}
                                    kifuTree={navigation.tree}
                                    onNodeClick={navigation.goToNodeById}
                                    onBranchSwitch={navigation.switchBranchAtNode}
                                    onAnalyzeNode={handleAnalyzeNode}
                                    onAnalyzeBranch={handleAnalyzeBranch}
                                    onStartTreeBatchAnalysis={handleStartTreeBatchAnalysis}
                                    isOnMainLine={navigation.state.isOnMainLine}
                                    onMoveDetailSelect={handleMoveDetailSelect}
                                />

                                {/* 詳細ドロワー（棋譜パネルの右側にスライドイン） */}
                                <div
                                    className={`
                                        absolute top-0 left-full z-50
                                        transform transition-transform duration-300 ease-out
                                        ${selectedMoveDetail ? "translate-x-0" : "-translate-x-full opacity-0 pointer-events-none"}
                                    `}
                                >
                                    <div className="pl-2">
                                        {selectedMoveDetail && (
                                            <MoveDetailPanel
                                                move={selectedMoveDetail.move}
                                                position={selectedMoveDetail.position}
                                                onAddBranch={handleAddPvAsBranch}
                                                onPreview={handlePreviewPv}
                                                onAnalyze={handleAnalyzePly}
                                                isAnalyzing={isAnalyzing}
                                                analyzingPly={
                                                    analyzingState.type !== "none"
                                                        ? analyzingState.ply
                                                        : undefined
                                                }
                                                kifuTree={navigation.tree}
                                                onClose={() => setSelectedMoveDetail(null)}
                                                isOnMainLine={navigation.state.isOnMainLine}
                                            />
                                        )}
                                    </div>
                                </div>
                            </div>
                        </div>

                        {/* 設定モーダル */}
                        <SettingsModal
                            isOpen={isSettingsModalOpen}
                            onClose={() => setIsSettingsModalOpen(false)}
                        >
                            <div className="flex flex-col gap-6 min-w-[400px]">
                                {/* 対局設定 */}
                                <MatchSettingsPanel
                                    sides={sides}
                                    onSidesChange={setSides}
                                    timeSettings={timeSettings}
                                    onTimeSettingsChange={setTimeSettings}
                                    passRightsSettings={passRightsSettings}
                                    onPassRightsSettingsChange={handlePassRightsSettingsChange}
                                    uiEngineOptions={uiEngineOptions}
                                    settingsLocked={settingsLocked}
                                />

                                {/* インポート */}
                                <KifuImportPanel
                                    onImportSfen={importSfen}
                                    onImportKif={importKif}
                                    positionReady={positionReady}
                                />

                                {/* エンジンログ（開発モード） */}
                                {isDevMode && (
                                    <EngineLogsPanel
                                        eventLogs={eventLogs}
                                        errorLogs={errorLogs}
                                        engineErrorDetails={engineErrorDetails}
                                        onRetry={retryEngine}
                                        isRetrying={isRetrying}
                                    />
                                )}
                            </div>
                        </SettingsModal>
                    </div>
                </section>
            )}
        </TooltipProvider>
    );
}
