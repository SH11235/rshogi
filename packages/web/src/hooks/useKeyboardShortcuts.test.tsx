import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";

// テスト用のコンポーネント
function TestComponent({ config }: { config: Parameters<typeof useKeyboardShortcuts>[0] }) {
    useKeyboardShortcuts(config);
    return <div>Test Component</div>;
}

describe("useKeyboardShortcuts", () => {
    it("should call onUndo when Ctrl+Z is pressed", () => {
        const onUndo = vi.fn();
        render(<TestComponent config={{ onUndo }} />);

        const event = new KeyboardEvent("keydown", {
            key: "z",
            ctrlKey: true,
            bubbles: true,
        });
        document.dispatchEvent(event);

        expect(onUndo).toHaveBeenCalledOnce();
    });

    it("should call onRedo when Ctrl+Y is pressed", () => {
        const onRedo = vi.fn();
        render(<TestComponent config={{ onRedo }} />);

        const event = new KeyboardEvent("keydown", {
            key: "y",
            ctrlKey: true,
            bubbles: true,
        });
        document.dispatchEvent(event);

        expect(onRedo).toHaveBeenCalledOnce();
    });

    it("should call onRedo when Ctrl+Shift+Z is pressed", () => {
        const onRedo = vi.fn();
        render(<TestComponent config={{ onRedo }} />);

        const event = new KeyboardEvent("keydown", {
            key: "Z",
            ctrlKey: true,
            shiftKey: true,
            bubbles: true,
        });
        document.dispatchEvent(event);

        expect(onRedo).toHaveBeenCalledOnce();
    });

    it("should call onReset when R is pressed", () => {
        const onReset = vi.fn();
        render(<TestComponent config={{ onReset }} />);

        const event = new KeyboardEvent("keydown", {
            key: "r",
            bubbles: true,
        });
        document.dispatchEvent(event);

        expect(onReset).toHaveBeenCalledOnce();
    });

    it("should call onEscape when Escape is pressed", () => {
        const onEscape = vi.fn();
        render(<TestComponent config={{ onEscape }} />);

        const event = new KeyboardEvent("keydown", {
            key: "Escape",
            bubbles: true,
        });
        document.dispatchEvent(event);

        expect(onEscape).toHaveBeenCalledOnce();
    });

    it("should call onEnter when Enter is pressed", () => {
        const onEnter = vi.fn();
        render(<TestComponent config={{ onEnter }} />);

        const event = new KeyboardEvent("keydown", {
            key: "Enter",
            bubbles: true,
        });
        document.dispatchEvent(event);

        expect(onEnter).toHaveBeenCalledOnce();
    });

    it("should not trigger shortcuts when input is focused", () => {
        const onUndo = vi.fn();
        render(<TestComponent config={{ onUndo }} />);

        // Create and focus an input element
        const input = document.createElement("input");
        document.body.appendChild(input);
        input.focus();

        const event = new KeyboardEvent("keydown", {
            key: "z",
            ctrlKey: true,
            bubbles: true,
        });
        document.dispatchEvent(event);

        expect(onUndo).not.toHaveBeenCalled();

        // Cleanup
        document.body.removeChild(input);
    });

    it("should not call callbacks when they are undefined", () => {
        render(<TestComponent config={{}} />);

        // These should not throw errors
        const events = [
            new KeyboardEvent("keydown", { key: "z", ctrlKey: true, bubbles: true }),
            new KeyboardEvent("keydown", { key: "y", ctrlKey: true, bubbles: true }),
            new KeyboardEvent("keydown", { key: "r", bubbles: true }),
            new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
            new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
        ];

        for (const event of events) {
            expect(() => document.dispatchEvent(event)).not.toThrow();
        }
    });
});
