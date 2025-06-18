import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { KeyboardHelp } from "./KeyboardHelp";

describe("KeyboardHelp", () => {
    it("should render help button", () => {
        render(<KeyboardHelp />);

        const button = screen.getByRole("button", { name: /ヘルプ/ });
        expect(button).toBeInTheDocument();
    });

    it("should show keyboard shortcuts when button is clicked", async () => {
        const user = userEvent.setup();
        render(<KeyboardHelp />);

        const button = screen.getByRole("button", { name: /ヘルプ/ });
        await user.click(button);

        // Check if dialog is opened
        expect(screen.getByText("キーボードショートカット")).toBeInTheDocument();

        // Check if all shortcuts are displayed
        expect(screen.getByText("Ctrl + Z")).toBeInTheDocument();
        expect(screen.getByText("元に戻す")).toBeInTheDocument();
        expect(screen.getByText("Ctrl + Y")).toBeInTheDocument();
        expect(screen.getByText("やり直し")).toBeInTheDocument();
        expect(screen.getByText("R")).toBeInTheDocument();
        expect(screen.getByText("ゲームリセット")).toBeInTheDocument();
        expect(screen.getByText("Escape")).toBeInTheDocument();
        expect(screen.getByText("選択解除・ダイアログクローズ")).toBeInTheDocument();
        expect(screen.getByText("Enter")).toBeInTheDocument();
        expect(screen.getByText("プロモーション時に成る")).toBeInTheDocument();
    });

    it("should close dialog when close button is clicked", async () => {
        const user = userEvent.setup();
        render(<KeyboardHelp />);

        // Open dialog
        const button = screen.getByRole("button", { name: /ヘルプ/ });
        await user.click(button);

        expect(screen.getByText("キーボードショートカット")).toBeInTheDocument();

        // Close dialog
        const closeButton = screen.getByRole("button", { name: "閉じる" });
        await user.click(closeButton);

        expect(screen.queryByText("キーボードショートカット")).not.toBeInTheDocument();
    });
});
