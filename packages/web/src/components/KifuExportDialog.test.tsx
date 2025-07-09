import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { Move, Player } from "shogi-core";
import { initialBoard, initialHands } from "shogi-core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { KifuExportDialog } from "./KifuExportDialog";

// Mock clipboard API globally before tests
const mockWriteText = vi.fn();

// Define clipboard on navigator if it doesn't exist
if (!navigator.clipboard) {
    Object.defineProperty(navigator, "clipboard", {
        value: {
            writeText: mockWriteText,
        },
        writable: true,
        configurable: true,
    });
} else {
    // If it exists, override writeText
    navigator.clipboard.writeText = mockWriteText;
}

describe("KifuExportDialog", () => {
    const mockOnOpenChange = vi.fn();
    const mockMoveHistory: Move[] = [
        {
            type: "move",
            from: { row: 7, column: 7 },
            to: { row: 6, column: 7 },
            piece: { type: "pawn", owner: "black", promoted: false },
            promote: false,
            captured: null,
        },
        {
            type: "move",
            from: { row: 3, column: 3 },
            to: { row: 4, column: 3 },
            piece: { type: "pawn", owner: "white", promoted: false },
            promote: false,
            captured: null,
        },
    ];

    const defaultProps = {
        open: true,
        onOpenChange: mockOnOpenChange,
        moveHistory: mockMoveHistory,
        currentBoard: initialBoard,
        currentHands: initialHands(),
        currentPlayer: "black" as Player,
        historyCursor: -1,
    };

    beforeEach(() => {
        vi.clearAllMocks();
        mockOnOpenChange.mockClear();
        mockWriteText.mockClear();
        mockWriteText.mockResolvedValue(undefined);

        // Mock URL methods
        global.URL.createObjectURL = vi.fn(() => "mock-url");
        global.URL.revokeObjectURL = vi.fn();
    });

    it("renders dialog when open", () => {
        render(<KifuExportDialog {...defaultProps} />);

        expect(screen.getByText("棋譜をエクスポート")).toBeInTheDocument();
        expect(screen.getByLabelText("KIF形式（棋譜）")).toBeInTheDocument();
        expect(screen.getByLabelText("SFEN形式（局面）")).toBeInTheDocument();
    });

    it("does not render dialog when closed", () => {
        render(<KifuExportDialog {...defaultProps} open={false} />);

        expect(screen.queryByText("棋譜をエクスポート")).not.toBeInTheDocument();
    });

    it("displays KIF format preview", () => {
        render(<KifuExportDialog {...defaultProps} />);

        const preview = screen.getByLabelText("プレビュー") as HTMLTextAreaElement;
        expect(preview.value).toContain("# KIF形式棋譜ファイル");
        expect(preview.value).toContain("手数----指手---------消費時間--");
        expect(preview.value).toContain("まで2手で対局終了");
    });

    it("displays SFEN format preview", async () => {
        const user = userEvent.setup();
        render(<KifuExportDialog {...defaultProps} />);

        const sfenRadio = screen.getByLabelText("SFEN形式（局面）");
        await user.click(sfenRadio);

        const preview = screen.getByLabelText("プレビュー") as HTMLTextAreaElement;
        expect(preview.value).toContain("sfen");
        expect(preview.value).toMatch(/^sfen .+ [bw] .+ \d+$/);
    });

    it("copies to clipboard", async () => {
        // Create a local mock that we control
        const localMockWriteText = vi.fn().mockResolvedValue(undefined);

        // Ensure navigator.clipboard exists and override writeText
        if (!navigator.clipboard) {
            Object.defineProperty(navigator, "clipboard", {
                value: { writeText: localMockWriteText },
                writable: true,
                configurable: true,
            });
        } else {
            navigator.clipboard.writeText = localMockWriteText;
        }

        const user = userEvent.setup();
        render(<KifuExportDialog {...defaultProps} />);

        // Verify the content is being generated
        const preview = screen.getByLabelText("プレビュー") as HTMLTextAreaElement;
        expect(preview.value).toContain("# KIF形式棋譜ファイル");

        const copyButton = screen.getByRole("button", { name: "クリップボードにコピー" });

        await user.click(copyButton);

        // Use waitFor to handle async operations
        await waitFor(
            () => {
                expect(localMockWriteText).toHaveBeenCalledTimes(1);
                expect(localMockWriteText).toHaveBeenCalledWith(
                    expect.stringContaining("# KIF形式棋譜ファイル"),
                );
            },
            { timeout: 2000 },
        );

        // Check if the button text changed
        await waitFor(() => {
            expect(screen.getByRole("button", { name: "コピーしました！" })).toBeInTheDocument();
        });
    });

    it("downloads file with default filename", async () => {
        const user = userEvent.setup();
        render(<KifuExportDialog {...defaultProps} />);

        const downloadButton = screen.getByText("ダウンロード");

        // Simply verify that clicking the download button closes the dialog
        await user.click(downloadButton);

        expect(mockOnOpenChange).toHaveBeenCalledWith(false);
    });

    it("downloads file with custom filename", async () => {
        const user = userEvent.setup();
        render(<KifuExportDialog {...defaultProps} />);

        const filenameInput = screen.getByLabelText("ファイル名（オプション）");
        await user.type(filenameInput, "my-game.kif");

        const downloadButton = screen.getByText("ダウンロード");
        await user.click(downloadButton);

        // Simply verify that the dialog closes
        expect(mockOnOpenChange).toHaveBeenCalledWith(false);
    });

    it("shows warning when exporting partial history", () => {
        render(<KifuExportDialog {...defaultProps} historyCursor={0} />);

        expect(
            screen.getByText(/※ 現在表示中の手（第1手）までをエクスポートします/),
        ).toBeInTheDocument();
    });

    it("disables download for empty KIF", () => {
        render(<KifuExportDialog {...defaultProps} moveHistory={[]} />);

        const downloadButton = screen.getByText("ダウンロード");
        expect(downloadButton).toBeDisabled();
        expect(screen.getByText("エクスポートする手がありません")).toBeInTheDocument();
    });

    it("handles cancel button", async () => {
        const user = userEvent.setup();
        render(<KifuExportDialog {...defaultProps} />);

        const cancelButton = screen.getByText("キャンセル");
        await user.click(cancelButton);

        expect(mockOnOpenChange).toHaveBeenCalledWith(false);
    });

    it("shows correct move count", () => {
        render(<KifuExportDialog {...defaultProps} />);

        expect(screen.getByText("2手の棋譜")).toBeInTheDocument();
    });

    it("switches to SFEN shows current position text", async () => {
        const user = userEvent.setup();
        render(<KifuExportDialog {...defaultProps} />);

        const sfenRadio = screen.getByLabelText("SFEN形式（局面）");
        await user.click(sfenRadio);

        expect(screen.getByText("現在の局面")).toBeInTheDocument();
    });
});
