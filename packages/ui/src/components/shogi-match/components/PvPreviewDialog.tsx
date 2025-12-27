/**
 * PVプレビューダイアログ
 *
 * 読み筋（PV）を独立した将棋盤で再生できるモーダル
 */

import type { BoardState, PositionState, Square } from "@shogi/app-core";
import { applyMoveWithState, boardToMatrix } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "../../dialog";
import type { ShogiBoardCell } from "../../shogi-board";
import { ShogiBoard } from "../../shogi-board";
import type { SquareNotation } from "../types";
import type { PvDisplayMove } from "../utils/kifFormat";
import { convertPvToDisplay } from "../utils/kifFormat";
import { HandPiecesDisplay } from "./HandPiecesDisplay";

interface PvPreviewDialogProps {
    /** ダイアログが開いているか */
    open: boolean;
    /** 閉じるコールバック */
    onClose: () => void;
    /** PV（USI形式の指し手配列） */
    pv: string[];
    /** 開始局面 */
    startPosition: PositionState;
    /** 手数（何手目のPVか） */
    ply: number;
    /** 評価値（センチポーン） */
    evalCp?: number;
    /** 詰み手数 */
    evalMate?: number;
    /** マス内座標表示形式 */
    squareNotation?: SquareNotation;
    /** 盤外ラベル表示 */
    showBoardLabels?: boolean;
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

export function PvPreviewDialog({
    open,
    onClose,
    pv,
    startPosition,
    ply,
    evalCp,
    evalMate,
    squareNotation = "none",
    showBoardLabels = false,
}: PvPreviewDialogProps): ReactElement {
    // 現在のプレビュー位置（0 = 開始局面、1 = PVの1手目後、...）
    const [previewIndex, setPreviewIndex] = useState(0);

    // 各ステップの局面を事前計算（有効な手のみ）
    const { positions, validPv } = useMemo((): {
        positions: PositionState[];
        validPv: string[];
    } => {
        const positionResult: PositionState[] = [startPosition];
        const validMoves: string[] = [];
        let currentPosition = startPosition;

        for (const move of pv) {
            const moveResult = applyMoveWithState(currentPosition, move, { validateTurn: false });
            if (!moveResult.ok) {
                // 無効な手以降は無視（エンジンPVの既知の動作）
                break;
            }
            validMoves.push(move);
            positionResult.push(moveResult.next);
            currentPosition = moveResult.next;
        }

        return { positions: positionResult, validPv: validMoves };
    }, [pv, startPosition]);

    // 有効なPVを表示用に変換
    const pvDisplay = useMemo((): PvDisplayMove[] => {
        return convertPvToDisplay(validPv, startPosition);
    }, [validPv, startPosition]);

    // 最終手情報
    const lastMove = useMemo(() => {
        if (previewIndex === 0) return undefined;
        const move = validPv[previewIndex - 1];
        if (!move) return undefined;

        // 打ち駒の場合
        if (move.includes("*")) {
            const to = move.slice(-2) as Square;
            return { from: undefined, to };
        }

        // 移動の場合
        const from = move.slice(0, 2) as Square;
        const to = move.slice(2, 4) as Square;
        return { from, to };
    }, [validPv, previewIndex]);

    // 現在の局面
    const currentPosition = useMemo(() => {
        return positions[previewIndex] ?? startPosition;
    }, [positions, previewIndex, startPosition]);

    // 盤面グリッド
    const grid = useMemo(() => {
        return boardToGrid(currentPosition.board);
    }, [currentPosition]);

    // キーボード操作
    const handleKeyDown = useCallback(
        (e: KeyboardEvent) => {
            if (!open) return;

            if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
                e.preventDefault();
                setPreviewIndex((prev) => Math.max(0, prev - 1));
            } else if (e.key === "ArrowRight" || e.key === "ArrowDown") {
                e.preventDefault();
                setPreviewIndex((prev) => Math.min(positions.length - 1, prev + 1));
            } else if (e.key === "Home") {
                e.preventDefault();
                setPreviewIndex(0);
            } else if (e.key === "End") {
                e.preventDefault();
                setPreviewIndex(positions.length - 1);
            } else if (e.key === "Escape") {
                e.preventDefault();
                onClose();
            }
        },
        [open, positions.length, onClose],
    );

    // キーボードイベントをリッスン
    useEffect(() => {
        if (open) {
            window.addEventListener("keydown", handleKeyDown);
            return () => window.removeEventListener("keydown", handleKeyDown);
        }
    }, [open, handleKeyDown]);

    // ダイアログを開いたときにリセット
    useEffect(() => {
        if (open) {
            setPreviewIndex(0);
        }
    }, [open]);

    // 評価値の表示フォーマット
    const evalText = useMemo(() => {
        if (evalMate !== undefined && evalMate !== null) {
            return evalMate > 0 ? `詰み${evalMate}手` : `被詰み${Math.abs(evalMate)}手`;
        }
        if (evalCp !== undefined && evalCp !== null) {
            const sign = evalCp >= 0 ? "+" : "";
            return `${sign}${evalCp}`;
        }
        return null;
    }, [evalCp, evalMate]);

    return (
        <Dialog open={open} onOpenChange={(isOpen) => !isOpen && onClose()}>
            <DialogContent className="max-w-[500px]">
                <DialogHeader className="flex flex-row items-center justify-between pr-8">
                    <DialogTitle className="text-sm font-medium">
                        {ply}手目の読み筋
                        <span className="text-muted-foreground font-normal">
                            （{validPv.length}手）
                        </span>
                        {evalText && (
                            <span className="ml-2 text-xs font-normal text-muted-foreground">
                                [{evalText}]
                            </span>
                        )}
                    </DialogTitle>
                </DialogHeader>
                {/* 右上の閉じるボタン */}
                <button
                    type="button"
                    onClick={onClose}
                    className="absolute right-3 top-3 p-1.5 rounded hover:bg-muted cursor-pointer text-muted-foreground hover:text-foreground"
                    aria-label="閉じる"
                >
                    <svg
                        xmlns="http://www.w3.org/2000/svg"
                        width="20"
                        height="20"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="2"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                    >
                        <line x1="18" y1="6" x2="6" y2="18" />
                        <line x1="6" y1="6" x2="18" y2="18" />
                    </svg>
                </button>

                <div className="flex flex-col gap-1.5">
                    {/* 将棋盤と持ち駒（コンパクト表示） */}
                    <div
                        className="flex flex-col items-center origin-top"
                        style={{ transform: "scale(0.85)", marginBottom: "-80px" }}
                    >
                        {/* 後手の持ち駒 */}
                        <div className="w-full flex justify-center">
                            <div className="text-base text-muted-foreground mr-2 self-center">
                                ☖
                            </div>
                            <HandPiecesDisplay
                                owner="gote"
                                hand={currentPosition.hands.gote}
                                selectedPiece={null}
                                isActive={false}
                                onHandSelect={() => {}}
                                flipBoard={false}
                            />
                        </div>

                        {/* 将棋盤 */}
                        <ShogiBoard
                            grid={grid}
                            selectedSquare={null}
                            lastMove={lastMove}
                            promotionSquare={null}
                            onSelect={() => {}}
                            flipBoard={false}
                            squareNotation={squareNotation}
                            showBoardLabels={showBoardLabels}
                        />

                        {/* 先手の持ち駒 */}
                        <div className="w-full flex justify-center">
                            <div className="text-base text-muted-foreground mr-2 self-center">
                                ☗
                            </div>
                            <HandPiecesDisplay
                                owner="sente"
                                hand={currentPosition.hands.sente}
                                selectedPiece={null}
                                isActive={false}
                                onHandSelect={() => {}}
                                flipBoard={false}
                            />
                        </div>
                    </div>

                    {/* ナビゲーション: 手数表示とボタンを1行に */}
                    <div className="flex items-center justify-center gap-1.5">
                        <button
                            type="button"
                            onClick={() => setPreviewIndex(0)}
                            disabled={previewIndex === 0}
                            className="px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded border border-border disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                        >
                            |&lt;
                        </button>
                        <button
                            type="button"
                            onClick={() => setPreviewIndex((prev) => Math.max(0, prev - 1))}
                            disabled={previewIndex === 0}
                            className="px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded border border-border disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                        >
                            &lt;
                        </button>
                        <span className="text-xs text-muted-foreground min-w-[80px] text-center">
                            {previewIndex === 0
                                ? "開始局面"
                                : `${previewIndex}/${validPv.length}手`}
                        </span>
                        <button
                            type="button"
                            onClick={() =>
                                setPreviewIndex((prev) => Math.min(positions.length - 1, prev + 1))
                            }
                            disabled={previewIndex >= positions.length - 1}
                            className="px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded border border-border disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                        >
                            &gt;
                        </button>
                        <button
                            type="button"
                            onClick={() => setPreviewIndex(positions.length - 1)}
                            disabled={previewIndex >= positions.length - 1}
                            className="px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded border border-border disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                        >
                            &gt;|
                        </button>
                    </div>

                    {/* 読み筋リスト */}
                    <div className="flex flex-wrap gap-0.5 text-[11px] font-mono justify-center">
                        {pvDisplay.map((move, index) => (
                            <button
                                key={`${index}-${move.usiMove}`}
                                type="button"
                                onClick={() => setPreviewIndex(index + 1)}
                                className={`px-1 py-0.5 rounded cursor-pointer ${
                                    index + 1 === previewIndex ? "bg-accent" : "hover:bg-muted"
                                } ${
                                    move.turn === "sente"
                                        ? "text-wafuu-shu"
                                        : "text-[hsl(210_70%_45%)]"
                                }`}
                            >
                                {move.displayText}
                            </button>
                        ))}
                    </div>
                </div>
            </DialogContent>
        </Dialog>
    );
}
