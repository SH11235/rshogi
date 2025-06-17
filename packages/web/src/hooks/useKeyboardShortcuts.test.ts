import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";

// Mock functions
const mockOnUndo = vi.fn();
const mockOnRedo = vi.fn();

describe("useKeyboardShortcuts", () => {
    beforeEach(() => {
        mockOnUndo.mockClear();
        mockOnRedo.mockClear();
    });

    it("should call onUndo when Ctrl+Z is pressed and canUndo is true", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: true,
                canRedo: false,
            }),
        );

        // Simulate Ctrl+Z keydown event
        const event = new KeyboardEvent("keydown", {
            key: "z",
            ctrlKey: true,
        });
        document.dispatchEvent(event);

        expect(mockOnUndo).toHaveBeenCalledTimes(1);
        expect(mockOnRedo).not.toHaveBeenCalled();
    });

    it("should call onRedo when Ctrl+Y is pressed and canRedo is true", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: false,
                canRedo: true,
            }),
        );

        // Simulate Ctrl+Y keydown event
        const event = new KeyboardEvent("keydown", {
            key: "y",
            ctrlKey: true,
        });
        document.dispatchEvent(event);

        expect(mockOnRedo).toHaveBeenCalledTimes(1);
        expect(mockOnUndo).not.toHaveBeenCalled();
    });

    it("should call onUndo when Cmd+Z is pressed on Mac and canUndo is true", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: true,
                canRedo: false,
            }),
        );

        // Simulate Cmd+Z keydown event (Mac)
        const event = new KeyboardEvent("keydown", {
            key: "z",
            metaKey: true,
        });
        document.dispatchEvent(event);

        expect(mockOnUndo).toHaveBeenCalledTimes(1);
        expect(mockOnRedo).not.toHaveBeenCalled();
    });

    it("should call onRedo when Cmd+Shift+Z is pressed on Mac and canRedo is true", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: false,
                canRedo: true,
            }),
        );

        // Simulate Cmd+Shift+Z keydown event (Mac)
        const event = new KeyboardEvent("keydown", {
            key: "z",
            metaKey: true,
            shiftKey: true,
        });
        document.dispatchEvent(event);

        expect(mockOnRedo).toHaveBeenCalledTimes(1);
        expect(mockOnUndo).not.toHaveBeenCalled();
    });

    it("should not call onUndo when Ctrl+Z is pressed but canUndo is false", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: false,
                canRedo: false,
            }),
        );

        const event = new KeyboardEvent("keydown", {
            key: "z",
            ctrlKey: true,
        });
        document.dispatchEvent(event);

        expect(mockOnUndo).not.toHaveBeenCalled();
        expect(mockOnRedo).not.toHaveBeenCalled();
    });

    it("should not call onRedo when Ctrl+Y is pressed but canRedo is false", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: false,
                canRedo: false,
            }),
        );

        const event = new KeyboardEvent("keydown", {
            key: "y",
            ctrlKey: true,
        });
        document.dispatchEvent(event);

        expect(mockOnUndo).not.toHaveBeenCalled();
        expect(mockOnRedo).not.toHaveBeenCalled();
    });

    it("should ignore keyboard shortcuts when focus is on input elements", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: true,
                canRedo: true,
            }),
        );

        // Create a mock input element
        const input = document.createElement("input");
        document.body.appendChild(input);
        input.focus();

        // Simulate Ctrl+Z keydown event on input
        const event = new KeyboardEvent("keydown", {
            key: "z",
            ctrlKey: true,
        });
        Object.defineProperty(event, "target", {
            value: input,
            enumerable: true,
        });
        document.dispatchEvent(event);

        expect(mockOnUndo).not.toHaveBeenCalled();
        expect(mockOnRedo).not.toHaveBeenCalled();

        // Clean up
        document.body.removeChild(input);
    });

    it("should ignore keyboard shortcuts when focus is on textarea elements", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: true,
                canRedo: true,
            }),
        );

        // Create a mock textarea element
        const textarea = document.createElement("textarea");
        document.body.appendChild(textarea);
        textarea.focus();

        // Simulate Ctrl+Z keydown event on textarea
        const event = new KeyboardEvent("keydown", {
            key: "z",
            ctrlKey: true,
        });
        Object.defineProperty(event, "target", {
            value: textarea,
            enumerable: true,
        });
        document.dispatchEvent(event);

        expect(mockOnUndo).not.toHaveBeenCalled();
        expect(mockOnRedo).not.toHaveBeenCalled();

        // Clean up
        document.body.removeChild(textarea);
    });

    it("should prevent default behavior when keyboard shortcuts are triggered", () => {
        renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: true,
                canRedo: true,
            }),
        );

        // Create spy for preventDefault
        const preventDefaultSpy = vi.fn();

        // Simulate Ctrl+Z keydown event
        const event = new KeyboardEvent("keydown", {
            key: "z",
            ctrlKey: true,
        });

        // Mock preventDefault
        Object.defineProperty(event, "preventDefault", {
            value: preventDefaultSpy,
            enumerable: true,
        });

        document.dispatchEvent(event);

        expect(preventDefaultSpy).toHaveBeenCalledTimes(1);
        expect(mockOnUndo).toHaveBeenCalledTimes(1);
    });

    it("should cleanup event listeners on unmount", () => {
        const removeEventListenerSpy = vi.spyOn(document, "removeEventListener");

        const { unmount } = renderHook(() =>
            useKeyboardShortcuts({
                onUndo: mockOnUndo,
                onRedo: mockOnRedo,
                canUndo: true,
                canRedo: true,
            }),
        );

        unmount();

        expect(removeEventListenerSpy).toHaveBeenCalledWith("keydown", expect.any(Function));

        removeEventListenerSpy.mockRestore();
    });
});
