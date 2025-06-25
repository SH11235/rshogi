import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { p2pStore } from "./p2pStore";

describe("p2pStore", () => {
    let mockListener: () => void;
    let unsubscribe: () => void;

    beforeEach(() => {
        // Clear any existing messages
        p2pStore.getSnapshot().length = 0;
        mockListener = vi.fn();
        unsubscribe = p2pStore.subscribe(mockListener);
    });

    afterEach(() => {
        unsubscribe();
        // Clean up event listeners
        window.removeEventListener("p2p-message", () => {});
    });

    it("should start with empty messages", () => {
        const messages = p2pStore.getSnapshot();
        expect(messages).toEqual([]);
    });

    it("should add message when p2p-message event is dispatched", () => {
        const testMessage = "Hello P2P!";
        const event = new CustomEvent("p2p-message", { detail: testMessage });

        window.dispatchEvent(event);

        const messages = p2pStore.getSnapshot();
        expect(messages).toHaveLength(1);
        expect(messages[0]).toBe(testMessage);
    });

    it("should notify listeners when message is received", () => {
        const testMessage = "Test notification";
        const event = new CustomEvent("p2p-message", { detail: testMessage });

        window.dispatchEvent(event);

        expect(mockListener).toHaveBeenCalledTimes(1);
    });

    it("should handle multiple messages", () => {
        const messages = ["Message 1", "Message 2", "Message 3"];

        for (const msg of messages) {
            const event = new CustomEvent("p2p-message", { detail: msg });
            window.dispatchEvent(event);
        }

        const receivedMessages = p2pStore.getSnapshot();
        expect(receivedMessages).toHaveLength(3);
        expect(receivedMessages).toEqual(messages);
    });

    it("should notify listeners for each message", () => {
        const messageCount = 5;

        for (let i = 0; i < messageCount; i++) {
            const event = new CustomEvent("p2p-message", { detail: `Message ${i}` });
            window.dispatchEvent(event);
        }

        expect(mockListener).toHaveBeenCalledTimes(messageCount);
    });

    it("should handle subscription and unsubscription correctly", () => {
        // Unsubscribe the initial listener
        unsubscribe();

        // Dispatch a message
        const event = new CustomEvent("p2p-message", { detail: "After unsubscribe" });
        window.dispatchEvent(event);

        // Listener should not have been called
        expect(mockListener).not.toHaveBeenCalled();

        // But the message should still be stored
        const messages = p2pStore.getSnapshot();
        expect(messages).toHaveLength(1);
    });

    it("should support multiple listeners", () => {
        const listener2 = vi.fn();
        const listener3 = vi.fn();

        const unsubscribe2 = p2pStore.subscribe(listener2);
        const unsubscribe3 = p2pStore.subscribe(listener3);

        const event = new CustomEvent("p2p-message", { detail: "Multi listener test" });
        window.dispatchEvent(event);

        expect(mockListener).toHaveBeenCalledTimes(1);
        expect(listener2).toHaveBeenCalledTimes(1);
        expect(listener3).toHaveBeenCalledTimes(1);

        unsubscribe2();
        unsubscribe3();
    });

    it("should maintain message order", () => {
        const orderedMessages = ["First", "Second", "Third", "Fourth"];

        for (const msg of orderedMessages) {
            const event = new CustomEvent("p2p-message", { detail: msg });
            window.dispatchEvent(event);
        }

        const receivedMessages = p2pStore.getSnapshot();
        expect(receivedMessages).toEqual(orderedMessages);
    });
});
