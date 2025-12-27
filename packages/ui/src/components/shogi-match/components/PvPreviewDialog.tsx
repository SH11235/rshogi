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

    return (
        <Dialog open={open} onOpenChange={(isOpen) => !isOpen && onClose()}>
            <DialogContent className="max-w-[500px]">
                <DialogHeader>
                    <DialogTitle className="text-base font-medium">
                        {ply}手目の読み筋
                        <span className="text-muted-foreground font-normal">
                            （{validPv.length}手）
                        </span>
                    </DialogTitle>
                </DialogHeader>

                <div className="flex flex-col gap-4">
                    {/* 将棋盤と持ち駒 */}
                    <div className="flex flex-col items-center gap-2">
                        {/* 後手の持ち駒 */}
                        <div className="w-full flex justify-center">
                            <div className="text-[11px] text-muted-foreground mr-2 self-center">
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
                            squareNotation="japanese"
                            showBoardLabels={true}
                        />

                        {/* 先手の持ち駒 */}
                        <div className="w-full flex justify-center">
                            <div className="text-[11px] text-muted-foreground mr-2 self-center">
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

                    {/* 手数表示 */}
                    <div className="text-center text-sm text-muted-foreground">
                        {previewIndex === 0
                            ? `${ply}手目の局面（読み筋開始前）`
                            : `読み筋 ${previewIndex}/${validPv.length} 手目`}
                    </div>

                    {/* ナビゲーションボタン */}
                    <div className="flex justify-center gap-2">
                        <button
                            type="button"
                            onClick={() => setPreviewIndex(0)}
                            disabled={previewIndex === 0}
                            className="px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded border border-border disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                        >
                            |&lt;
                        </button>
                        <button
                            type="button"
                            onClick={() => setPreviewIndex((prev) => Math.max(0, prev - 1))}
                            disabled={previewIndex === 0}
                            className="px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded border border-border disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                        >
                            &lt;
                        </button>
                        <button
                            type="button"
                            onClick={() =>
                                setPreviewIndex((prev) => Math.min(positions.length - 1, prev + 1))
                            }
                            disabled={previewIndex >= positions.length - 1}
                            className="px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded border border-border disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                        >
                            &gt;
                        </button>
                        <button
                            type="button"
                            onClick={() => setPreviewIndex(positions.length - 1)}
                            disabled={previewIndex >= positions.length - 1}
                            className="px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded border border-border disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                        >
                            &gt;|
                        </button>
                    </div>

                    {/* 読み筋リスト */}
                    <div className="flex flex-wrap gap-1 text-[12px] font-mono justify-center">
                        {pvDisplay.map((move, index) => (
                            <button
                                key={`${index}-${move.usiMove}`}
                                type="button"
                                onClick={() => setPreviewIndex(index + 1)}
                                className={`px-1.5 py-0.5 rounded cursor-pointer ${
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

                    {/* 操作のヒント */}
                    <div className="text-center text-[11px] text-muted-foreground space-y-0.5">
                        <div>PC: 矢印キーで前後移動 / Home/Endで最初/最後 / Escで閉じる</div>
                        <div>スマホ: 指し手をタップで移動 / ◀▶ボタンで前後移動</div>
                    </div>
                </div>
            </DialogContent>
        </Dialog>
    );
}
