import type { BoardState, Piece, PositionState, Square } from "@shogi/app-core";
import { cloneBoard, createEmptyHands, getAllSquares } from "@shogi/app-core";
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, type Mock, vi } from "vitest";
import { LegalMoveCache } from "../utils/legalMoveCache";
import { type UsePositionEditorProps, usePositionEditor } from "./usePositionEditor";

describe("usePositionEditor", () => {
    let mockProps: UsePositionEditorProps;
    let initialPosition: PositionState;
    let initialBoard: BoardState;

    beforeEach(() => {
        // 空の盤面を作成
        initialBoard = Object.fromEntries(getAllSquares().map((sq) => [sq, null])) as BoardState;

        initialPosition = {
            board: initialBoard,
            hands: createEmptyHands(),
            turn: "sente",
            ply: 1,
        };

        mockProps = {
            initialPosition,
            initialBoard,
            isMatchRunning: false,
            onPositionChange: vi.fn(),
            onInitialBoardChange: vi.fn(),
            onMovesChange: vi.fn(),
            onLastMoveChange: vi.fn(),
            onSelectionChange: vi.fn(),
            onMessageChange: vi.fn(),
            onStartSfenRefresh: vi.fn().mockResolvedValue(undefined),
            legalCache: new LegalMoveCache(),
            matchEndedRef: { current: false },
            onMatchRunningChange: vi.fn(),
            positionRef: { current: initialPosition },
            movesRef: { current: [] },
            onSearchStatesReset: vi.fn(),
            onActiveSearchReset: vi.fn(),
            onClockStop: vi.fn(),
            onBasePositionChange: vi.fn(),
        };
    });

    describe("placePieceAt", () => {
        it("空マスに駒を配置できる", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const piece: Piece = {
                owner: "sente",
                type: "P",
            };

            act(() => {
                const success = result.current.placePieceAt("5e" as Square, piece);
                expect(success).toBe(true);
            });

            expect(mockProps.onPositionChange).toHaveBeenCalled();
        });

        it("駒のあるマスに駒を配置すると、既存駒が手駒に回収される", () => {
            // 5eに先手の歩を配置
            const boardWithPiece = cloneBoard(initialBoard);
            boardWithPiece["5e"] = { owner: "sente", type: "P" };
            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWithPiece,
            };

            const { result } = renderHook(() => usePositionEditor(mockProps));

            // 5eに後手の金を配置
            const piece: Piece = {
                owner: "gote",
                type: "G",
            };

            act(() => {
                const success = result.current.placePieceAt("5e" as Square, piece);
                expect(success).toBe(true);
            });

            // onPositionChange が呼ばれ、hands に歩が追加されている
            expect(mockProps.onPositionChange).toHaveBeenCalled();
            const calledPosition = (mockProps.onPositionChange as Mock).mock
                .calls[0][0] as PositionState;
            expect(calledPosition.hands.sente.P).toBe(1);
        });

        it("null を渡すと駒を削除できる", () => {
            // 5eに先手の歩を配置
            const boardWithPiece = cloneBoard(initialBoard);
            boardWithPiece["5e"] = { owner: "sente", type: "P" };
            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWithPiece,
            };

            const { result } = renderHook(() => usePositionEditor(mockProps));

            act(() => {
                const success = result.current.placePieceAt("5e" as Square, null);
                expect(success).toBe(true);
            });

            const calledPosition = (mockProps.onPositionChange as Mock).mock
                .calls[0][0] as PositionState;
            expect(calledPosition.board["5e"]).toBeNull();
            expect(calledPosition.hands.sente.P).toBe(1); // 手駒に回収される
        });

        it("fromSquare を指定すると駒を移動できる", () => {
            // 5eに先手の歩を配置
            const boardWithPiece = cloneBoard(initialBoard);
            boardWithPiece["5e"] = { owner: "sente", type: "P" };
            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWithPiece,
            };

            const { result } = renderHook(() => usePositionEditor(mockProps));

            const piece: Piece = {
                owner: "sente",
                type: "P",
            };

            act(() => {
                const success = result.current.placePieceAt("5d" as Square, piece, {
                    fromSquare: "5e" as Square,
                });
                expect(success).toBe(true);
            });

            const calledPosition = (mockProps.onPositionChange as Mock).mock
                .calls[0][0] as PositionState;
            expect(calledPosition.board["5e"]).toBeNull(); // 移動元は空
            expect(calledPosition.board["5d"]).toEqual({ owner: "sente", type: "P" });
        });

        it("成駒を配置できる", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const piece: Piece = {
                owner: "sente",
                type: "P",
                promoted: true,
            };

            act(() => {
                const success = result.current.placePieceAt("5e" as Square, piece);
                expect(success).toBe(true);
            });

            const calledPosition = (mockProps.onPositionChange as Mock).mock
                .calls[0][0] as PositionState;
            expect(calledPosition.board["5e"]).toEqual({
                owner: "sente",
                type: "P",
                promoted: true,
            });
        });
    });

    describe("piece count limits", () => {
        it("歩は18枚まで配置可能", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            // 17枚配置（1枚は手駒）
            const boardWith17Pawns = cloneBoard(initialBoard);
            for (let i = 1; i <= 9; i++) {
                boardWith17Pawns[`${i}a` as Square] = { owner: "sente", type: "P" };
            }
            for (let i = 1; i <= 8; i++) {
                boardWith17Pawns[`${i}b` as Square] = { owner: "sente", type: "P" };
            }

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith17Pawns,
                hands: { sente: { P: 1 }, gote: createEmptyHands().gote },
            };

            const piece: Piece = { owner: "sente", type: "P" };

            // 18枚目は配置可能
            act(() => {
                const success = result.current.placePieceAt("9b" as Square, piece);
                expect(success).toBe(true);
            });
        });

        it("歩は19枚目を配置できない", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            // 18枚配置
            const boardWith18Pawns = cloneBoard(initialBoard);
            for (let i = 1; i <= 9; i++) {
                boardWith18Pawns[`${i}a` as Square] = { owner: "sente", type: "P" };
                boardWith18Pawns[`${i}b` as Square] = { owner: "sente", type: "P" };
            }

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith18Pawns,
            };

            const piece: Piece = { owner: "sente", type: "P" };

            // 19枚目は配置不可
            act(() => {
                const success = result.current.placePieceAt("5c" as Square, piece);
                expect(success).toBe(false);
            });

            expect(mockProps.onMessageChange).toHaveBeenCalledWith(
                expect.stringContaining("最大18枚"),
            );
        });

        it("金は4枚まで配置可能", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWith3Golds = cloneBoard(initialBoard);
            boardWith3Golds["5a"] = { owner: "sente", type: "G" };
            boardWith3Golds["6a"] = { owner: "sente", type: "G" };
            boardWith3Golds["7a"] = { owner: "sente", type: "G" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith3Golds,
            };

            const piece: Piece = { owner: "sente", type: "G" };

            // 4枚目は配置可能
            act(() => {
                const success = result.current.placePieceAt("8a" as Square, piece);
                expect(success).toBe(true);
            });
        });

        it("金は5枚目を配置できない", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWith4Golds = cloneBoard(initialBoard);
            boardWith4Golds["5a"] = { owner: "sente", type: "G" };
            boardWith4Golds["6a"] = { owner: "sente", type: "G" };
            boardWith4Golds["7a"] = { owner: "sente", type: "G" };
            boardWith4Golds["8a"] = { owner: "sente", type: "G" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith4Golds,
            };

            const piece: Piece = { owner: "sente", type: "G" };

            // 5枚目は配置不可
            act(() => {
                const success = result.current.placePieceAt("9a" as Square, piece);
                expect(success).toBe(false);
            });

            expect(mockProps.onMessageChange).toHaveBeenCalledWith(
                expect.stringContaining("最大4枚"),
            );
        });

        it("銀は4枚まで配置可能", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWith3Silvers = cloneBoard(initialBoard);
            boardWith3Silvers["5a"] = { owner: "sente", type: "S" };
            boardWith3Silvers["6a"] = { owner: "sente", type: "S" };
            boardWith3Silvers["7a"] = { owner: "sente", type: "S" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith3Silvers,
            };

            const piece: Piece = { owner: "sente", type: "S" };

            act(() => {
                const success = result.current.placePieceAt("8a" as Square, piece);
                expect(success).toBe(true);
            });
        });

        it("桂は4枚まで配置可能", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWith3Knights = cloneBoard(initialBoard);
            boardWith3Knights["5a"] = { owner: "sente", type: "N" };
            boardWith3Knights["6a"] = { owner: "sente", type: "N" };
            boardWith3Knights["7a"] = { owner: "sente", type: "N" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith3Knights,
            };

            const piece: Piece = { owner: "sente", type: "N" };

            act(() => {
                const success = result.current.placePieceAt("8a" as Square, piece);
                expect(success).toBe(true);
            });
        });

        it("香は4枚まで配置可能", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWith3Lances = cloneBoard(initialBoard);
            boardWith3Lances["5a"] = { owner: "sente", type: "L" };
            boardWith3Lances["6a"] = { owner: "sente", type: "L" };
            boardWith3Lances["7a"] = { owner: "sente", type: "L" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith3Lances,
            };

            const piece: Piece = { owner: "sente", type: "L" };

            act(() => {
                const success = result.current.placePieceAt("8a" as Square, piece);
                expect(success).toBe(true);
            });
        });

        it("角は2枚まで配置可能", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWith1Bishop = cloneBoard(initialBoard);
            boardWith1Bishop["5a"] = { owner: "sente", type: "B" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith1Bishop,
            };

            const piece: Piece = { owner: "sente", type: "B" };

            act(() => {
                const success = result.current.placePieceAt("6a" as Square, piece);
                expect(success).toBe(true);
            });
        });

        it("飛は2枚まで配置可能", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWith1Rook = cloneBoard(initialBoard);
            boardWith1Rook["5a"] = { owner: "sente", type: "R" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith1Rook,
            };

            const piece: Piece = { owner: "sente", type: "R" };

            act(() => {
                const success = result.current.placePieceAt("6a" as Square, piece);
                expect(success).toBe(true);
            });
        });

        it("玉は1枚まで配置可能", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const piece: Piece = { owner: "sente", type: "K" };

            act(() => {
                const success = result.current.placePieceAt("5i" as Square, piece);
                expect(success).toBe(true);
            });
        });

        it("玉は2枚目を配置できない", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWith1King = cloneBoard(initialBoard);
            boardWith1King["5i"] = { owner: "sente", type: "K" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWith1King,
            };

            const piece: Piece = { owner: "sente", type: "K" };

            act(() => {
                const success = result.current.placePieceAt("6i" as Square, piece);
                expect(success).toBe(false);
            });

            expect(mockProps.onMessageChange).toHaveBeenCalledWith(
                expect.stringContaining("1枚まで"),
            );
        });

        it("先手と後手で別々にカウントされる", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            const boardWithBothKings = cloneBoard(initialBoard);
            boardWithBothKings["5i"] = { owner: "sente", type: "K" };

            mockProps.positionRef.current = {
                ...initialPosition,
                board: boardWithBothKings,
            };

            const piece: Piece = { owner: "gote", type: "K" };

            // 後手の玉は配置可能
            act(() => {
                const success = result.current.placePieceAt("5a" as Square, piece);
                expect(success).toBe(true);
            });
        });
    });

    describe("board operations", () => {
        it("updateTurnForEdit が手番を更新する", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            act(() => {
                result.current.updateTurnForEdit("gote");
            });

            expect(mockProps.onPositionChange).toHaveBeenCalled();
            const calledPosition = (mockProps.onPositionChange as Mock).mock
                .calls[0][0] as PositionState;
            expect(calledPosition.turn).toBe("gote");
        });

        it("updateTurnForEdit は対局中は実行されない", () => {
            mockProps.isMatchRunning = true;

            const { result } = renderHook(() => usePositionEditor(mockProps));

            act(() => {
                result.current.updateTurnForEdit("gote");
            });

            expect(mockProps.onPositionChange).not.toHaveBeenCalled();
        });
    });

    describe("edit state management", () => {
        it("編集状態を管理できる", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            expect(result.current.isEditMode).toBe(true);
            expect(result.current.editOwner).toBe("sente");
            expect(result.current.editPieceType).toBeNull();
            expect(result.current.editPromoted).toBe(false);
            expect(result.current.editFromSquare).toBeNull();
            expect(result.current.editTool).toBe("place");
        });

        it("編集状態を更新できる", () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            act(() => {
                result.current.setEditOwner("gote");
                result.current.setEditPieceType("G");
                result.current.setEditPromoted(true);
                result.current.setEditTool("erase");
            });

            expect(result.current.editOwner).toBe("gote");
            expect(result.current.editPieceType).toBe("G");
            expect(result.current.editPromoted).toBe(true);
            expect(result.current.editTool).toBe("erase");
        });

        it("finalizeEditedPosition が編集を確定する", async () => {
            const { result } = renderHook(() => usePositionEditor(mockProps));

            await act(async () => {
                await result.current.finalizeEditedPosition();
            });

            expect(result.current.isEditMode).toBe(false);
            expect(mockProps.onBasePositionChange).toHaveBeenCalled();
            expect(mockProps.onStartSfenRefresh).toHaveBeenCalled();
        });

        it("finalizeEditedPosition は対局中は実行されない", async () => {
            mockProps.isMatchRunning = true;

            const { result } = renderHook(() => usePositionEditor(mockProps));

            await act(async () => {
                await result.current.finalizeEditedPosition();
            });

            expect(mockProps.onBasePositionChange).not.toHaveBeenCalled();
        });
    });
});
