import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useP2P } from "./useP2P";

// Create a promise that we can control
let wasmInitResolve: () => void;
let wasmInitPromise: Promise<void>;

// Mock the WASM module
vi.mock("../wasm/shogi_core", () => {
    return {
        default: vi.fn(() => Promise.resolve()),
        start_p2p_manager: vi.fn(() => ({
            send_message: vi.fn(),
            free: () => {},
        })),
    };
});

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
    let mockWasmInit: ReturnType<typeof vi.fn>;
    let mockStartP2PManager: ReturnType<typeof vi.fn>;
    let mockSendMessage: ReturnType<typeof vi.fn>;

    beforeEach(async () => {
        vi.clearAllMocks();
        const globals = globalThis as unknown as TestGlobals;
        globals.__p2pMessages = [];

        // Reset the promise for each test
        wasmInitPromise = new Promise((resolve) => {
            wasmInitResolve = resolve;
        });

        // Get mocked functions
        const wasmModule = await import("../wasm/shogi_core");
        mockWasmInit = vi.mocked(wasmModule.default);
        mockStartP2PManager = vi.mocked(wasmModule.start_p2p_manager);
        mockSendMessage = vi.fn();

        // Setup mock implementations
        mockWasmInit.mockReturnValue(wasmInitPromise);
        mockStartP2PManager.mockReturnValue({
            send_message: mockSendMessage,
            free: () => {},
        });
    });

    afterEach(() => {
        vi.clearAllMocks();
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

        expect(mockStartP2PManager).toHaveBeenCalledWith("test-room-123");
        expect(result.current.isConnected).toBe(true);
    });

    it("should not connect if already connected", async () => {
        const { result } = renderHook(() => useP2P());

        // Initialize WASM first
        await act(async () => {
            wasmInitResolve();
            await wasmInitPromise;
        });

        // Connect first time
        act(() => {
            result.current.connect("test-room");
        });

        expect(mockStartP2PManager).toHaveBeenCalledTimes(1);

        // Try to connect again
        act(() => {
            result.current.connect("another-room");
        });

        // Should only be called once
        expect(mockStartP2PManager).toHaveBeenCalledTimes(1);
        expect(mockStartP2PManager).toHaveBeenCalledWith("test-room");
    });

    it("should not connect before initialization", () => {
        const { result } = renderHook(() => useP2P());

        // Try to connect without initializing WASM
        act(() => {
            result.current.connect("test-room");
        });

        expect(mockStartP2PManager).not.toHaveBeenCalled();
        expect(result.current.isConnected).toBe(false);
    });

    it("should send messages when connected", async () => {
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
        const { result } = renderHook(() => useP2P());

        // Initialize WASM
        await act(async () => {
            wasmInitResolve();
            await wasmInitPromise;
        });

        // Try to send without connecting
        act(() => {
            result.current.sendMessage("Hello!");
        });

        expect(mockSendMessage).not.toHaveBeenCalled();
    });

    it("should receive messages from store", () => {
        const globals = globalThis as unknown as TestGlobals;
        globals.__p2pMessages = ["Message 1", "Message 2"];

        const { result } = renderHook(() => useP2P());

        expect(result.current.receivedMessages).toEqual(["Message 1", "Message 2"]);
    });

    it("should update when new messages arrive", () => {
        const globals = globalThis as unknown as TestGlobals;
        globals.__p2pMessages = [];

        const { result } = renderHook(() => useP2P());

        expect(result.current.receivedMessages).toEqual([]);

        // Simulate a new message
        act(() => {
            globals.__p2pMessages = ["New message"];
            if (globals.__p2pStoreCallback) {
                globals.__p2pStoreCallback();
            }
        });

        expect(result.current.receivedMessages).toEqual(["New message"]);
    });
});
