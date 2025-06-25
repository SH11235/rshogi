import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useP2P } from "./useP2P";

// Create a promise that we can control
let wasmInitResolve: () => void;
let wasmInitPromise: Promise<void>;

// Mock the WASM module
vi.mock("shogi-rust-core", () => ({
    default: vi.fn(() => wasmInitPromise),
    start_p2p_manager: vi.fn(() => ({
        send_message: vi.fn(),
    })),
}));

// Type for test globals
interface TestGlobals {
    __p2pStoreCallback?: () => void;
    __p2pMessages?: string[];
}

// Mock the p2pStore
vi.mock("../stores/p2pStore", () => ({
    p2pStore: {
        subscribe: vi.fn((callback) => {
            // Store the callback for testing
            const globals = globalThis as unknown as TestGlobals;
            globals.__p2pStoreCallback = callback;
            return () => {
                globals.__p2pStoreCallback = undefined;
            };
        }),
        getSnapshot: vi.fn(() => {
            const globals = globalThis as unknown as TestGlobals;
            return globals.__p2pMessages || [];
        }),
    },
}));

describe("useP2P", () => {
    beforeEach(() => {
        vi.clearAllMocks();
        const globals = globalThis as unknown as TestGlobals;
        globals.__p2pMessages = [];
        // Reset the promise for each test
        wasmInitPromise = new Promise((resolve) => {
            wasmInitResolve = resolve;
        });
    });

    afterEach(() => {
        const globals = globalThis as unknown as TestGlobals;
        globals.__p2pMessages = undefined;
        globals.__p2pStoreCallback = undefined;
    });

    it("should initialize with default state", async () => {
        const { result } = renderHook(() => useP2P());

        expect(result.current.isInitialized).toBe(false);
        expect(result.current.isConnected).toBe(false);
        expect(result.current.receivedMessages).toEqual([]);

        // Resolve the WASM initialization
        await act(async () => {
            wasmInitResolve();
            await wasmInitPromise;
        });

        expect(result.current.isInitialized).toBe(true);
    });

    it("should connect to a room", async () => {
        const { result } = renderHook(() => useP2P());
        const { start_p2p_manager } = await import("shogi-rust-core");
        const mockStart = vi.mocked(start_p2p_manager);

        // Initialize WASM first
        await act(async () => {
            wasmInitResolve();
            await wasmInitPromise;
        });

        expect(result.current.isInitialized).toBe(true);

        // Connect to a room
        act(() => {
            result.current.connect("test-room-123");
        });

        expect(mockStart).toHaveBeenCalledWith("test-room-123");
        expect(result.current.isConnected).toBe(true);
    });

    it("should not connect if already connected", async () => {
        const { result } = renderHook(() => useP2P());
        const { start_p2p_manager } = await import("shogi-rust-core");
        const mockStart = vi.mocked(start_p2p_manager);

        // Initialize WASM
        await act(async () => {
            wasmInitResolve();
            await wasmInitPromise;
        });

        // Connect once
        act(() => {
            result.current.connect("test-room");
        });

        // Try to connect again
        act(() => {
            result.current.connect("another-room");
        });

        // Should only be called once
        expect(mockStart).toHaveBeenCalledTimes(1);
        expect(mockStart).toHaveBeenCalledWith("test-room");
    });

    it("should not connect before initialization", async () => {
        const { result } = renderHook(() => useP2P());
        const { start_p2p_manager } = await import("shogi-rust-core");
        const mockStart = vi.mocked(start_p2p_manager);

        // Try to connect before initialization (WASM not resolved)
        act(() => {
            result.current.connect("test-room");
        });

        expect(mockStart).not.toHaveBeenCalled();
        expect(result.current.isConnected).toBe(false);
    });

    it("should send messages when connected", async () => {
        const mockSendMessage = vi.fn();
        const { start_p2p_manager } = await import("shogi-rust-core");
        const mockStart = vi.mocked(start_p2p_manager);
        mockStart.mockReturnValue({
            send_message: mockSendMessage,
            free: () => {},
        } as ReturnType<typeof start_p2p_manager>);

        const { result } = renderHook(() => useP2P());

        // Initialize WASM
        await act(async () => {
            wasmInitResolve();
            await wasmInitPromise;
        });

        // Connect
        act(() => {
            result.current.connect("test-room");
        });

        // Send a message
        act(() => {
            result.current.sendMessage("Hello P2P!");
        });

        expect(mockSendMessage).toHaveBeenCalledWith("Hello P2P!");
    });

    it("should not send messages when not connected", async () => {
        const mockSendMessage = vi.fn();
        const { start_p2p_manager } = await import("shogi-rust-core");
        const mockStart = vi.mocked(start_p2p_manager);
        mockStart.mockReturnValue({
            send_message: mockSendMessage,
            free: () => {},
        } as ReturnType<typeof start_p2p_manager>);

        const { result } = renderHook(() => useP2P());

        // Initialize WASM
        await act(async () => {
            wasmInitResolve();
            await wasmInitPromise;
        });

        // Try to send without connecting
        const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
        act(() => {
            result.current.sendMessage("Should not send");
        });

        expect(mockSendMessage).not.toHaveBeenCalled();
        expect(consoleSpy).toHaveBeenCalledWith(
            "Cannot send message, not connected or handle not ready.",
        );

        consoleSpy.mockRestore();
    });

    it("should receive messages from store", async () => {
        const { result } = renderHook(() => useP2P());

        // Simulate receiving messages
        act(() => {
            const globals = globalThis as unknown as TestGlobals;
            globals.__p2pMessages = ["Message 1", "Message 2"];
            if (globals.__p2pStoreCallback) {
                globals.__p2pStoreCallback();
            }
        });

        expect(result.current.receivedMessages).toEqual(["Message 1", "Message 2"]);
    });

    it("should update when new messages arrive", async () => {
        const { result } = renderHook(() => useP2P());

        expect(result.current.receivedMessages).toEqual([]);

        // Add first message
        act(() => {
            const globals = globalThis as unknown as TestGlobals;
            globals.__p2pMessages = ["First message"];
            if (globals.__p2pStoreCallback) {
                globals.__p2pStoreCallback();
            }
        });

        expect(result.current.receivedMessages).toEqual(["First message"]);

        // Add second message
        act(() => {
            const globals = globalThis as unknown as TestGlobals;
            globals.__p2pMessages = ["First message", "Second message"];
            if (globals.__p2pStoreCallback) {
                globals.__p2pStoreCallback();
            }
        });

        expect(result.current.receivedMessages).toEqual(["First message", "Second message"]);
    });
});
