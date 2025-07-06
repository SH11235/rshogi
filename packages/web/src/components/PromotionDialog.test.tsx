import { fireEvent, render, screen } from "@testing-library/react";
import type { Piece } from "shogi-core";
import { describe, expect, it, vi } from "vitest";
import { PromotionDialog } from "./PromotionDialog";

describe("PromotionDialog", () => {
    const mockOnConfirm = vi.fn();
    const mockOnCancel = vi.fn();

    const blackPawn: Piece = {
        type: "pawn",
        owner: "black",
        promoted: false,
    };

    const whitePawn: Piece = {
        type: "pawn",
        owner: "white",
        promoted: false,
    };

    const silverGeneral: Piece = {
        type: "silver",
        owner: "black",
        promoted: false,
    };

    beforeEach(() => {
        mockOnConfirm.mockClear();
        mockOnCancel.mockClear();
    });

    it("表示されない場合、何もレンダリングしない", () => {
        const { container } = render(
            <PromotionDialog
                piece={blackPawn}
                isOpen={false}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );
        expect(container.firstChild).toBeNull();
    });

    it("表示される場合、ダイアログをレンダリングする", () => {
        render(
            <PromotionDialog
                piece={blackPawn}
                isOpen={true}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );

        expect(screen.getByText("駒を成りますか？")).toBeInTheDocument();
        expect(screen.getByText("歩")).toBeInTheDocument();
        expect(screen.getByText("と")).toBeInTheDocument();
        expect(screen.getByText("成る")).toBeInTheDocument();
        expect(screen.getByText("成らない")).toBeInTheDocument();
        expect(screen.getByText("移動をキャンセル")).toBeInTheDocument();
    });

    it("先手の駒の場合、黒色で表示される", () => {
        render(
            <PromotionDialog
                piece={blackPawn}
                isOpen={true}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );

        const pieces = screen.getAllByText("歩");
        expect(pieces[0]).toHaveClass("text-black");
    });

    it("後手の駒の場合、赤色で表示される", () => {
        render(
            <PromotionDialog
                piece={whitePawn}
                isOpen={true}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );

        const pieces = screen.getAllByText("歩");
        expect(pieces[0]).toHaveClass("text-red-600");
    });

    it("銀将の場合、正しい成り駒名を表示する", () => {
        render(
            <PromotionDialog
                piece={silverGeneral}
                isOpen={true}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );

        expect(screen.getByText("銀")).toBeInTheDocument();
        expect(screen.getByText("全")).toBeInTheDocument();
    });

    it("成るボタンをクリックすると、onConfirm(true)が呼ばれる", () => {
        render(
            <PromotionDialog
                piece={blackPawn}
                isOpen={true}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );

        fireEvent.click(screen.getByText("成る"));
        expect(mockOnConfirm).toHaveBeenCalledWith(true);
        expect(mockOnConfirm).toHaveBeenCalledTimes(1);
    });

    it("成らないボタンをクリックすると、onConfirm(false)が呼ばれる", () => {
        render(
            <PromotionDialog
                piece={blackPawn}
                isOpen={true}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );

        fireEvent.click(screen.getByText("成らない"));
        expect(mockOnConfirm).toHaveBeenCalledWith(false);
        expect(mockOnConfirm).toHaveBeenCalledTimes(1);
    });

    it("移動をキャンセルボタンをクリックすると、onCancelが呼ばれる", () => {
        render(
            <PromotionDialog
                piece={blackPawn}
                isOpen={true}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );

        fireEvent.click(screen.getByText("移動をキャンセル"));
        expect(mockOnCancel).toHaveBeenCalledTimes(1);
        expect(mockOnConfirm).not.toHaveBeenCalled();
    });

    it("背景のオーバーレイをクリックしても何も起こらない", () => {
        const { container } = render(
            <PromotionDialog
                piece={blackPawn}
                isOpen={true}
                onConfirm={mockOnConfirm}
                onCancel={mockOnCancel}
            />,
        );

        const overlay = container.querySelector(".bg-black.bg-opacity-50");
        expect(overlay).toBeInTheDocument();

        if (overlay) {
            fireEvent.click(overlay);
        }
        expect(mockOnConfirm).not.toHaveBeenCalled();
        expect(mockOnCancel).not.toHaveBeenCalled();
    });

    it("各駒の種類で正しい表示がされる", () => {
        const pieces: Array<{ piece: Piece; normal: string; promoted: string }> = [
            {
                piece: { type: "pawn", owner: "black", promoted: false },
                normal: "歩",
                promoted: "と",
            },
            {
                piece: { type: "lance", owner: "black", promoted: false },
                normal: "香",
                promoted: "成香",
            },
            {
                piece: { type: "knight", owner: "black", promoted: false },
                normal: "桂",
                promoted: "圭",
            },
            {
                piece: { type: "silver", owner: "black", promoted: false },
                normal: "銀",
                promoted: "全",
            },
            {
                piece: { type: "rook", owner: "black", promoted: false },
                normal: "飛",
                promoted: "龍",
            },
            {
                piece: { type: "bishop", owner: "black", promoted: false },
                normal: "角",
                promoted: "馬",
            },
        ];

        for (const { piece, normal, promoted } of pieces) {
            const { unmount } = render(
                <PromotionDialog
                    piece={piece}
                    isOpen={true}
                    onConfirm={mockOnConfirm}
                    onCancel={mockOnCancel}
                />,
            );

            expect(screen.getByText(normal)).toBeInTheDocument();
            expect(screen.getByText(promoted)).toBeInTheDocument();
            unmount();
        }
    });
});
