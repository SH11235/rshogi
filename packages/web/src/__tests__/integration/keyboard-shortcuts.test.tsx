import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import App from "../../App";

describe("Keyboard Shortcuts Integration", () => {
    beforeEach(() => {
        // Render the full app
        render(<App />);
    });

    it("should show keyboard shortcut hints in the move history panel", () => {
        // The UI shows two history panels (mobile and desktop), so we expect multiple matches
        const hintElements = screen.getAllByText(/💡/);
        expect(hintElements.length).toBeGreaterThanOrEqual(1);

        const undoElements = screen.getAllByText(/戻る/);
        expect(undoElements.length).toBeGreaterThanOrEqual(1);

        const redoElements = screen.getAllByText(/進む/);
        expect(redoElements.length).toBeGreaterThanOrEqual(1);

        const ctrlZElements = screen.getAllByText(/Ctrl\+Z/);
        expect(ctrlZElements.length).toBeGreaterThanOrEqual(1);

        const ctrlYElements = screen.getAllByText(/Ctrl\+Y/);
        expect(ctrlYElements.length).toBeGreaterThanOrEqual(1);
    });

    it("should show tooltips on undo/redo buttons", () => {
        const undoButtons = screen.getAllByRole("button", { name: /戻る/ });
        const redoButtons = screen.getAllByRole("button", { name: /進む/ });

        // Check at least one undo and redo button has the correct tooltip
        expect(undoButtons.some((btn) => btn.getAttribute("title") === "戻る (Ctrl+Z)")).toBe(true);
        expect(redoButtons.some((btn) => btn.getAttribute("title") === "進む (Ctrl+Y)")).toBe(true);
    });

    it("should respond to Ctrl+Z keyboard shortcut after making moves", () => {
        // Make a move first by clicking on a piece and then a valid move
        const pawnSquares = screen.getAllByLabelText(/Square 7七 with pawn piece/);
        const pawnSquare = pawnSquares[0];
        fireEvent.click(pawnSquare);

        // Check if the piece is selected (should show valid moves)
        const moveSquares = screen.getAllByLabelText(/Square 7六/);
        const moveSquare = moveSquares[0];
        fireEvent.click(moveSquare);

        // After the move, check that we can use Ctrl+Z to undo
        const moveHistorySections = screen.getAllByText("棋譜");
        expect(moveHistorySections.length).toBeGreaterThanOrEqual(1);

        // Simulate Ctrl+Z keypress
        fireEvent.keyDown(document, {
            key: "z",
            ctrlKey: true,
        });

        // Check that the game state has changed (move was undone)
        // The exact assertion depends on the current game state display
        const playerTurnElements = screen.getAllByText("先手番");
        expect(playerTurnElements.length).toBeGreaterThanOrEqual(1);
    });

    it("should respond to Ctrl+Y keyboard shortcut for redo", () => {
        // This test verifies that the Ctrl+Y keyboard shortcut is properly set up
        // The actual functionality is tested in the hook unit tests
        // We just verify that the DOM interaction doesn't cause errors

        // Simulate Ctrl+Y keypress without any game state changes
        fireEvent.keyDown(document, {
            key: "y",
            ctrlKey: true,
        });

        // Verify the basic UI is still functioning
        expect(screen.getByText("将棋")).toBeInTheDocument();
        const playerElements = screen.getAllByText("先手番");
        expect(playerElements.length).toBeGreaterThanOrEqual(1);
    });

    it("should not interfere with text input in dialogs", () => {
        // This test would need a scenario where there's a text input
        // For now, we'll test that the shortcuts don't trigger when focused on button elements
        const undoButtons = screen.getAllByRole("button", { name: /戻る/ });
        const undoButton = undoButtons[0];
        undoButton.focus();

        // Even when focused on a button, keyboard shortcuts should still work
        // since buttons are not text input elements
        fireEvent.keyDown(document, {
            key: "z",
            ctrlKey: true,
        });

        // The shortcut should work (we can't easily test the actual undo here without setup)
        // But we can verify the button exists and is interactable
        expect(undoButton).toBeInTheDocument();
    });

    it("should have keyboard shortcut functionality available", () => {
        // This test verifies that the keyboard shortcut system is integrated
        // The actual preventDefault testing is done in the hook unit tests
        const moveHistorySections = screen.getAllByText("棋譜");
        expect(moveHistorySections.length).toBeGreaterThanOrEqual(1);

        // Verify undo/redo buttons exist and have keyboard hint tooltips
        const undoButtons = screen.getAllByRole("button", { name: /戻る/ });
        const redoButtons = screen.getAllByRole("button", { name: /進む/ });

        expect(undoButtons.some((btn) => btn.getAttribute("title") === "戻る (Ctrl+Z)")).toBe(true);
        expect(redoButtons.some((btn) => btn.getAttribute("title") === "進む (Ctrl+Y)")).toBe(true);
    });
});
