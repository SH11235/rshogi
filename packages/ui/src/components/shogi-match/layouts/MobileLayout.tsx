import type { LastMove, PieceType, Player, PositionState, Square } from "@shogi/app-core";
import type { ReactElement, RefObject } from "react";
import { useMemo, useState } from "react";
import type { ShogiBoardCell } from "../../shogi-board";
import { BottomSheet } from "../components/BottomSheet";
import { EvalGraph } from "../components/EvalGraph";
import type { EngineOption, SideSetting } from "../components/MatchSettingsPanel";
import { MobileBoardSection } from "../components/MobileBoardSection";
import { MobileClockDisplay } from "../components/MobileClockDisplay";
import { type KifuMove, MobileKifuBar } from "../components/MobileKifuBar";
import { MobileNavigation } from "../components/MobileNavigation";
import { MobileSettingsSheet } from "../components/MobileSettingsSheet";
import type { ClockSettings, TickState } from "../hooks/useClockManager";
import type { DisplaySettings, GameMode, PromotionSelection } from "../types";
import type { EvalHistory } from "../utils/kifFormat";

// テキストスタイル用Tailwindクラス
const TEXT_CLASSES = {
    mutedSecondary: "text-xs text-muted-foreground",
    moveCount: "text-center text-sm font-semibold text-foreground",
} as const;

type Selection = { kind: "square"; square: string } | { kind: "hand"; piece: PieceType };

export interface MobileLayoutProps {
    // 盤面関連
    grid: ShogiBoardCell[][];
    position: PositionState;
    flipBoard: boolean;
    lastMove?: LastMove;
    selection: Selection | null;
    promotionSelection: PromotionSelection | null;
    isEditMode: boolean;
    isMatchRunning: boolean;
    gameMode: GameMode;
    editFromSquare: Square | null;
    moves: string[];
    candidateNote: string | null;

    // 表示設定
    displaySettings: Pick<
        DisplaySettings,
        "highlightLastMove" | "squareNotation" | "showBoardLabels"
    >;

    // イベントハンドラ
    onSquareSelect: (square: string, shiftKey?: boolean) => void;
    onPromotionChoice: (promote: boolean) => void;
    onFlipBoard: () => void;
    onHandSelect: (piece: PieceType) => void;

    // 編集モード用
    onPiecePointerDown?: (
        square: string,
        piece: { owner: "sente" | "gote"; type: string; promoted?: boolean },
        e: React.PointerEvent,
    ) => void;
    onPieceTogglePromote?: (
        square: string,
        piece: { owner: "sente" | "gote"; type: string; promoted?: boolean },
        event: React.MouseEvent<HTMLButtonElement>,
    ) => void;
    onHandPiecePointerDown?: (owner: Player, pieceType: PieceType, e: React.PointerEvent) => void;
    onIncrementHand?: (owner: Player, piece: PieceType) => void;
    onDecrementHand?: (owner: Player, piece: PieceType) => void;

    // 検討モード関連
    isReviewMode: boolean;

    // 棋譜関連
    kifMoves?: KifuMove[];
    currentPly?: number;
    totalPly?: number;
    onPlySelect?: (ply: number) => void;

    // ナビゲーション
    onBack?: () => void;
    onForward?: () => void;
    onToStart?: () => void;
    onToEnd?: () => void;

    // 評価値
    evalHistory: EvalHistory[];
    evalCp?: number;
    evalMate?: number;

    // 対局コントロール
    onStop?: () => void;
    onStart?: () => void;
    onResetToStartpos?: () => void;

    // 対局設定（モバイル用BottomSheet）
    sides: { sente: SideSetting; gote: SideSetting };
    onSidesChange: (sides: { sente: SideSetting; gote: SideSetting }) => void;
    timeSettings: ClockSettings;
    onTimeSettingsChange: (settings: ClockSettings) => void;
    onTurnChange: (turn: Player) => void;
    uiEngineOptions: EngineOption[];
    settingsLocked: boolean;

    // クロック表示
    clocks: TickState;

    // 表示設定（フル版、BottomSheet用）
    displaySettingsFull: DisplaySettings;
    onDisplaySettingsChange: (settings: DisplaySettings) => void;

    // 持ち駒情報取得
    getHandInfo: (pos: "top" | "bottom") => {
        owner: Player;
        hand: PositionState["hands"]["sente"] | PositionState["hands"]["gote"];
        isActive: boolean;
    };

    // Ref
    boardSectionRef: RefObject<HTMLDivElement | null>;

    // DnD関連
    isDraggingPiece: boolean;
}

/**
 * スマホ用レイアウト
 * 「盤面優先 + Flexbox」方式
 * - 盤面は画面幅から計算した固定サイズ
 * - コントロール部分は残りの高さを使い、必要に応じて縮小
 */
export function MobileLayout({
    grid,
    position,
    flipBoard,
    lastMove,
    selection,
    promotionSelection,
    isEditMode,
    isMatchRunning,
    gameMode,
    editFromSquare,
    moves,
    candidateNote,
    displaySettings,
    onSquareSelect,
    onPromotionChoice,
    onFlipBoard,
    onHandSelect,
    onPiecePointerDown,
    onPieceTogglePromote,
    onHandPiecePointerDown,
    onIncrementHand,
    onDecrementHand,
    isReviewMode,
    kifMoves,
    currentPly = 0,
    totalPly = 0,
    onPlySelect,
    onBack,
    onForward,
    onToStart,
    onToEnd,
    evalHistory,
    evalCp,
    evalMate,
    onStop,
    onStart,
    onResetToStartpos,
    sides,
    onSidesChange,
    timeSettings,
    onTimeSettingsChange,
    onTurnChange,
    uiEngineOptions,
    settingsLocked,
    clocks,
    displaySettingsFull,
    onDisplaySettingsChange,
    getHandInfo,
    boardSectionRef,
    isDraggingPiece,
}: MobileLayoutProps): ReactElement {
    // 設定BottomSheetの状態
    const [isSettingsOpen, setIsSettingsOpen] = useState(false);

    // 持ち駒情報を事前計算（useMemoで安定させてReact.memoを有効にする）
    const topHand = useMemo(() => getHandInfo("top"), [getHandInfo]);
    const bottomHand = useMemo(() => getHandInfo("bottom"), [getHandInfo]);

    // 編集モード判定を事前計算（MobileBoardSectionに渡す）
    const isEditModeActive = isEditMode && !isMatchRunning;

    return (
        <div className="fixed inset-0 flex flex-col w-full h-dvh overflow-hidden px-2 bg-background">
            {/* === ヘッダー: 自然な高さ、縮小しない === */}
            <header className="flex-shrink-0">
                {/* ステータス行 */}
                <div className="flex items-center justify-between w-full py-2 px-2">
                    <output className={`${TEXT_CLASSES.moveCount} whitespace-nowrap`}>
                        {moves.length === 0 ? "開始局面" : `${moves.length}手目`}
                    </output>

                    <output className={`${TEXT_CLASSES.mutedSecondary} whitespace-nowrap`}>
                        手番:{" "}
                        <span
                            className={`font-semibold text-[15px] ${
                                position.turn === "sente" ? "text-wafuu-shu" : "text-wafuu-ai"
                            }`}
                        >
                            {position.turn === "sente" ? "先手" : "後手"}
                        </span>
                    </output>

                    <button
                        type="button"
                        onClick={onFlipBoard}
                        className="flex items-center justify-center w-8 h-8 rounded-md hover:bg-muted"
                        title="盤面を反転"
                    >
                        🔄
                    </button>
                </div>

                {/* クロック表示（対局モード時は常に表示） */}
                {(isMatchRunning || gameMode === "playing") && (
                    <MobileClockDisplay clocks={clocks} sides={sides} isRunning={isMatchRunning} />
                )}
            </header>

            {/* === 盤面セクション: 固定サイズ、縮小しない === */}
            <main className="flex-shrink-0">
                <MobileBoardSection
                    grid={grid}
                    position={position}
                    flipBoard={flipBoard}
                    lastMove={lastMove}
                    selection={selection}
                    promotionSelection={promotionSelection}
                    displaySettings={displaySettings}
                    isEditModeActive={isEditModeActive}
                    editFromSquare={editFromSquare}
                    candidateNote={candidateNote}
                    onSquareSelect={onSquareSelect}
                    onPromotionChoice={onPromotionChoice}
                    onHandSelect={onHandSelect}
                    onPiecePointerDown={onPiecePointerDown}
                    onPieceTogglePromote={onPieceTogglePromote}
                    onHandPiecePointerDown={onHandPiecePointerDown}
                    onIncrementHand={onIncrementHand}
                    onDecrementHand={onDecrementHand}
                    topHand={topHand}
                    bottomHand={bottomHand}
                    boardSectionRef={boardSectionRef}
                    isDraggingPiece={isDraggingPiece}
                    fixedLayout={gameMode === "playing"}
                />
            </main>

            {/* === コントロール: 残りの高さを使う、必要に応じて縮小 === */}
            <footer className="flex-1 flex flex-col min-h-0 mt-2">
                {gameMode === "playing" ? (
                    /* 対局モード: 1行棋譜 + 停止ボタン */
                    <div className="flex flex-col gap-2 flex-shrink-0">
                        {kifMoves && kifMoves.length > 0 && (
                            <MobileKifuBar
                                moves={kifMoves}
                                currentPly={currentPly}
                                onPlySelect={onPlySelect}
                            />
                        )}
                        {onStop && (
                            <div className="flex justify-center py-2">
                                <button
                                    type="button"
                                    onClick={onStop}
                                    className="px-8 py-3 bg-destructive text-destructive-foreground rounded-lg font-medium shadow-md active:scale-95 transition-transform"
                                >
                                    停止
                                </button>
                            </div>
                        )}
                    </div>
                ) : isReviewMode && totalPly === 0 ? (
                    /* 対局準備モード: 開始ボタンのみ（棋譜がまだない状態） */
                    <div className="flex justify-center gap-3 py-4 flex-shrink-0">
                        {onStart && (
                            <button
                                type="button"
                                onClick={onStart}
                                className="px-8 py-3 bg-primary text-primary-foreground rounded-lg font-medium shadow-md active:scale-95 transition-transform"
                            >
                                対局を開始
                            </button>
                        )}
                    </div>
                ) : isReviewMode ? (
                    /* 検討モード: 評価値グラフ + ナビゲーション + 棋譜バー */
                    <div className="flex flex-col h-full min-h-0">
                        {/* 評価値グラフ + 現在の評価値: 縮小可能 */}
                        <div className="flex-shrink min-h-[60px] px-2 overflow-hidden">
                            <div className="flex items-center gap-2 mb-1">
                                <span className="text-xs text-muted-foreground">評価値:</span>
                                <span className="text-sm font-mono tabular-nums">
                                    {evalMate !== undefined
                                        ? evalMate > 0
                                            ? `詰み${evalMate}手`
                                            : `詰まされ${Math.abs(evalMate)}手`
                                        : evalCp !== undefined
                                          ? `${evalCp > 0 ? "+" : ""}${evalCp}`
                                          : "-"}
                                </span>
                            </div>
                            <EvalGraph
                                evalHistory={evalHistory}
                                currentPly={currentPly}
                                compact
                                height={50}
                            />
                        </div>

                        {/* ナビゲーションボタン: 縮小しない */}
                        {onBack && onForward && onToStart && onToEnd && (
                            <div className="flex-shrink-0 mt-1">
                                <MobileNavigation
                                    currentPly={currentPly}
                                    totalPly={totalPly}
                                    onBack={onBack}
                                    onForward={onForward}
                                    onToStart={onToStart}
                                    onToEnd={onToEnd}
                                    onSettingsClick={() => setIsSettingsOpen(true)}
                                />
                            </div>
                        )}

                        {/* 簡易棋譜表示: 縮小可能 */}
                        {kifMoves && kifMoves.length > 0 && (
                            <div className="flex-shrink min-h-[36px] mt-1">
                                <MobileKifuBar
                                    moves={kifMoves}
                                    currentPly={currentPly}
                                    onPlySelect={onPlySelect}
                                />
                            </div>
                        )}
                    </div>
                ) : (
                    /* 編集モード: 平手に戻す + 対局開始ボタン */
                    <div className="flex flex-col gap-2 flex-shrink-0">
                        <div className="text-center text-sm text-muted-foreground">
                            盤面をタップして編集
                        </div>
                        <div className="flex justify-center gap-3 py-2">
                            {onResetToStartpos && (
                                <button
                                    type="button"
                                    onClick={onResetToStartpos}
                                    className="px-4 py-2 border border-border rounded-lg text-sm font-medium hover:bg-muted active:scale-95 transition-all"
                                >
                                    平手に戻す
                                </button>
                            )}
                            {onStart && (
                                <button
                                    type="button"
                                    onClick={onStart}
                                    className="px-4 py-2 bg-primary text-primary-foreground rounded-lg text-sm font-medium shadow-md active:scale-95 transition-all"
                                >
                                    対局を開始
                                </button>
                            )}
                        </div>
                    </div>
                )}
            </footer>

            {/* FAB: 設定ボタン（右下固定）
                検討モードで棋譜がある場合は、ナビゲーションバーに設定ボタンがあるので非表示 */}
            {!(isReviewMode && totalPly > 0) && (
                <button
                    type="button"
                    onClick={() => setIsSettingsOpen(true)}
                    className="fixed bottom-4 right-4 w-14 h-14 rounded-full bg-primary text-primary-foreground shadow-lg flex items-center justify-center text-2xl active:scale-95 transition-transform z-40"
                    aria-label="対局設定を開く"
                >
                    ⚙️
                </button>
            )}

            {/* 設定BottomSheet */}
            <BottomSheet
                isOpen={isSettingsOpen}
                onClose={() => setIsSettingsOpen(false)}
                title="設定"
                height="auto"
            >
                <MobileSettingsSheet
                    sides={sides}
                    onSidesChange={onSidesChange}
                    timeSettings={timeSettings}
                    onTimeSettingsChange={onTimeSettingsChange}
                    currentTurn={position.turn}
                    onTurnChange={onTurnChange}
                    uiEngineOptions={uiEngineOptions}
                    settingsLocked={settingsLocked}
                    isMatchRunning={isMatchRunning}
                    onStartMatch={
                        onStart
                            ? () => {
                                  onStart();
                                  setIsSettingsOpen(false);
                              }
                            : undefined
                    }
                    onStopMatch={
                        onStop
                            ? () => {
                                  onStop();
                                  setIsSettingsOpen(false);
                              }
                            : undefined
                    }
                    onResetToStartpos={
                        onResetToStartpos
                            ? () => {
                                  onResetToStartpos();
                                  setIsSettingsOpen(false);
                              }
                            : undefined
                    }
                    displaySettings={displaySettingsFull}
                    onDisplaySettingsChange={onDisplaySettingsChange}
                />
            </BottomSheet>
        </div>
    );
}
