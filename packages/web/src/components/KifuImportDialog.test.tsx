import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { KifuImportDialog } from "./KifuImportDialog";

describe("KifuImportDialog", () => {
    const mockOnImport = vi.fn();
    const mockOnOpenChange = vi.fn();

    beforeEach(() => {
        vi.clearAllMocks();
        mockOnImport.mockClear();
        mockOnOpenChange.mockClear();
    });

    it("renders dialog when open", () => {
        render(
            <KifuImportDialog
                open={true}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        expect(screen.getByText("棋譜をインポート")).toBeInTheDocument();
        expect(screen.getByLabelText("KIF形式（棋譜）")).toBeInTheDocument();
        expect(screen.getByLabelText("SFEN形式（局面）")).toBeInTheDocument();
    });

    it("does not render dialog when closed", () => {
        render(
            <KifuImportDialog
                open={false}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        expect(screen.queryByText("棋譜をインポート")).not.toBeInTheDocument();
    });

    it("imports valid KIF format", async () => {
        const user = userEvent.setup();
        const kifContent = `# KIF形式棋譜ファイル
手数----指手---------消費時間--
   1 ７六歩(77)   ( 0:00/00:00:00)
   2 ３四歩(33)   ( 0:00/00:00:00)`;

        render(
            <KifuImportDialog
                open={true}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        const textarea = screen.getByPlaceholderText(/KIF形式の棋譜を貼り付けてください/);
        await user.type(textarea, kifContent);

        const importButton = screen.getByText("インポート");
        await user.click(importButton);

        await waitFor(() => {
            expect(mockOnImport).toHaveBeenCalledWith(
                expect.arrayContaining([
                    expect.objectContaining({
                        type: "move",
                        from: { row: 7, column: 7 },
                        to: { row: 6, column: 7 },
                    }),
                    expect.objectContaining({
                        type: "move",
                        from: { row: 3, column: 3 },
                        to: { row: 4, column: 3 },
                    }),
                ]),
                "kif",
                kifContent,
            );
        });

        expect(mockOnOpenChange).toHaveBeenCalledWith(false);
    });

    it("imports valid SFEN format", async () => {
        const user = userEvent.setup();
        const sfenContent = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

        render(
            <KifuImportDialog
                open={true}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        // SFEN形式を選択
        const sfenRadio = screen.getByLabelText("SFEN形式（局面）");
        await user.click(sfenRadio);

        const textarea = screen.getByPlaceholderText(/SFEN形式の局面を貼り付けてください/);
        await user.type(textarea, sfenContent);

        const importButton = screen.getByText("インポート");
        await user.click(importButton);

        await waitFor(() => {
            expect(mockOnImport).toHaveBeenCalledWith([], "sfen", sfenContent);
        });

        expect(mockOnOpenChange).toHaveBeenCalledWith(false);
    });

    it("shows error for invalid KIF format", async () => {
        const user = userEvent.setup();
        const invalidKif = "これは無効なKIF形式です";

        render(
            <KifuImportDialog
                open={true}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        const textarea = screen.getByPlaceholderText(/KIF形式の棋譜を貼り付けてください/);
        await user.type(textarea, invalidKif);

        const importButton = screen.getByText("インポート");
        await user.click(importButton);

        await waitFor(() => {
            expect(screen.getByText(/KIF形式のヘッダーが見つかりません/)).toBeInTheDocument();
        });

        expect(mockOnImport).not.toHaveBeenCalled();
    });

    it("shows error for invalid SFEN format", async () => {
        const user = userEvent.setup();
        const invalidSfen = "invalid sfen string";

        render(
            <KifuImportDialog
                open={true}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        const sfenRadio = screen.getByLabelText("SFEN形式（局面）");
        await user.click(sfenRadio);

        const textarea = screen.getByPlaceholderText(/SFEN形式の局面を貼り付けてください/);
        await user.type(textarea, invalidSfen);

        const importButton = screen.getByText("インポート");
        await user.click(importButton);

        await waitFor(() => {
            expect(screen.getByText(/無効なSFEN/)).toBeInTheDocument();
        });

        expect(mockOnImport).not.toHaveBeenCalled();
    });

    it("handles file selection", async () => {
        const user = userEvent.setup();
        const kifContent = `# KIF形式棋譜ファイル
手数----指手---------消費時間--
   1 ７六歩(77)   ( 0:00/00:00:00)`;

        const file = new File([kifContent], "test.kif", { type: "text/plain" });

        render(
            <KifuImportDialog
                open={true}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        const fileInput = screen
            .getByText("ファイルを選択")
            .parentElement?.querySelector("input[type='file']");
        expect(fileInput).toBeInTheDocument();

        if (fileInput) {
            await user.upload(fileInput as HTMLInputElement, file);
        }

        // ファイル読み込みを待つ
        await waitFor(() => {
            const textarea = screen.getByPlaceholderText(
                /KIF形式の棋譜を貼り付けてください/,
            ) as HTMLTextAreaElement;
            expect(textarea.value).toContain("手数----指手");
        });
    });

    it("handles cancel button", async () => {
        const user = userEvent.setup();

        render(
            <KifuImportDialog
                open={true}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        const cancelButton = screen.getByText("キャンセル");
        await user.click(cancelButton);

        expect(mockOnOpenChange).toHaveBeenCalledWith(false);
        expect(mockOnImport).not.toHaveBeenCalled();
    });

    it("detects format from file extension", async () => {
        const user = userEvent.setup();
        const sfenContent = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        const file = new File([sfenContent], "position.sfen", { type: "text/plain" });

        render(
            <KifuImportDialog
                open={true}
                onOpenChange={mockOnOpenChange}
                onImport={mockOnImport}
            />,
        );

        const fileInput = screen
            .getByText("ファイルを選択")
            .parentElement?.querySelector("input[type='file']");

        if (fileInput) {
            await user.upload(fileInput as HTMLInputElement, file);
        }

        await waitFor(() => {
            // SFEN形式が自動的に選択されることを確認
            const sfenRadio = screen.getByRole("radio", { name: "SFEN形式（局面）" });
            expect(sfenRadio).toHaveAttribute("aria-checked", "true");
        });
    });
});
