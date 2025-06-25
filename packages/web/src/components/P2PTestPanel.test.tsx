import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { P2PTestPanel } from "./P2PTestPanel";

// Mock the useP2P hook
const mockConnect = vi.fn();
const mockSendMessage = vi.fn();
const mockUseP2P = vi.fn();

vi.mock("../hooks/useP2P", () => ({
    useP2P: () => mockUseP2P(),
}));

describe("P2PTestPanel", () => {
    const defaultMockReturn = {
        isInitialized: true,
        isConnected: false,
        receivedMessages: [],
        connect: mockConnect,
        sendMessage: mockSendMessage,
    };

    beforeEach(() => {
        vi.clearAllMocks();
        mockUseP2P.mockReturnValue(defaultMockReturn);
    });

    it("should show initialization message when not initialized", () => {
        mockUseP2P.mockReturnValue({
            ...defaultMockReturn,
            isInitialized: false,
        });

        render(<P2PTestPanel />);
        expect(screen.getByText("Initializing Wasm...")).toBeInTheDocument();
    });

    it("should render connection form when initialized", () => {
        render(<P2PTestPanel />);

        expect(screen.getByText("P2P Test Panel")).toBeInTheDocument();
        expect(screen.getByPlaceholderText("Room ID")).toBeInTheDocument();
        expect(screen.getByRole("button", { name: "Connect" })).toBeInTheDocument();
        expect(screen.getByPlaceholderText("Message to send")).toBeInTheDocument();
        expect(screen.getByRole("button", { name: "Send" })).toBeInTheDocument();
    });

    it("should have default room ID", () => {
        render(<P2PTestPanel />);

        const roomInput = screen.getByPlaceholderText("Room ID") as HTMLInputElement;
        expect(roomInput.value).toBe("shogi-room-1");
    });

    it("should connect with room ID", async () => {
        const user = userEvent.setup();
        render(<P2PTestPanel />);

        const roomInput = screen.getByPlaceholderText("Room ID");
        const connectButton = screen.getByRole("button", { name: "Connect" });

        // Change room ID
        await user.clear(roomInput);
        await user.type(roomInput, "custom-room");

        // Click connect
        await user.click(connectButton);

        expect(mockConnect).toHaveBeenCalledWith("custom-room");
    });

    it("should disable inputs when connected", () => {
        mockUseP2P.mockReturnValue({
            ...defaultMockReturn,
            isConnected: true,
        });

        render(<P2PTestPanel />);

        const roomInput = screen.getByPlaceholderText("Room ID");
        const connectButton = screen.getByRole("button", { name: "Connected" });

        expect(roomInput).toBeDisabled();
        expect(connectButton).toBeDisabled();
        expect(connectButton).toHaveTextContent("Connected");
    });

    it("should send messages when connected", async () => {
        const user = userEvent.setup();
        mockUseP2P.mockReturnValue({
            ...defaultMockReturn,
            isConnected: true,
        });

        render(<P2PTestPanel />);

        const messageInput = screen.getByPlaceholderText("Message to send");
        const sendButton = screen.getByRole("button", { name: "Send" });

        // Type and send message
        await user.type(messageInput, "Hello P2P World!");
        await user.click(sendButton);

        expect(mockSendMessage).toHaveBeenCalledWith("Hello P2P World!");
    });

    it("should clear message input after sending", async () => {
        const user = userEvent.setup();
        mockUseP2P.mockReturnValue({
            ...defaultMockReturn,
            isConnected: true,
        });

        render(<P2PTestPanel />);

        const messageInput = screen.getByPlaceholderText("Message to send") as HTMLInputElement;
        const sendButton = screen.getByRole("button", { name: "Send" });

        await user.type(messageInput, "Test message");
        expect(messageInput.value).toBe("Test message");

        await user.click(sendButton);
        expect(messageInput.value).toBe("");
    });

    it("should disable message input and send button when not connected", () => {
        render(<P2PTestPanel />);

        const messageInput = screen.getByPlaceholderText("Message to send");
        const sendButton = screen.getByRole("button", { name: "Send" });

        expect(messageInput).toBeDisabled();
        expect(sendButton).toBeDisabled();
    });

    it("should display received messages", () => {
        mockUseP2P.mockReturnValue({
            ...defaultMockReturn,
            receivedMessages: ["Message 1", "Message 2", "Message 3"],
        });

        render(<P2PTestPanel />);

        expect(screen.getByText("Received Messages:")).toBeInTheDocument();
        expect(screen.getByText("Message 1")).toBeInTheDocument();
        expect(screen.getByText("Message 2")).toBeInTheDocument();
        expect(screen.getByText("Message 3")).toBeInTheDocument();
    });

    it("should render with proper styling", () => {
        render(<P2PTestPanel />);

        const panel = screen.getByText("P2P Test Panel").closest("div");
        expect(panel).toHaveStyle({
            border: "1px solid black",
            padding: "10px",
            margin: "10px",
        });
    });

    it("should handle empty message submission", async () => {
        const user = userEvent.setup();
        mockUseP2P.mockReturnValue({
            ...defaultMockReturn,
            isConnected: true,
        });

        render(<P2PTestPanel />);

        const sendButton = screen.getByRole("button", { name: "Send" });
        await user.click(sendButton);

        expect(mockSendMessage).toHaveBeenCalledWith("");
    });
});
