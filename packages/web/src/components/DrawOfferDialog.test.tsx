import { useGameStore } from "@/stores/gameStore";
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { DrawOfferDialog } from "./DrawOfferDialog";

// useGameStoreのモック
vi.mock("@/stores/gameStore");

const mockUseGameStore = vi.mocked(useGameStore);

describe("DrawOfferDialog", () => {
    const mockAcceptDrawOffer = vi.fn();
    const mockRejectDrawOffer = vi.fn();
    const mockOfferDraw = vi.fn();

    const defaultStore = {
        drawOfferPending: false,
        pendingDrawOfferer: null,
        localPlayer: "black",
        currentPlayer: "black",
        acceptDrawOffer: mockAcceptDrawOffer,
        rejectDrawOffer: mockRejectDrawOffer,
        offerDraw: mockOfferDraw,
        gameStatus: "playing",
        isOnlineGame: false,
        connectionStatus: { isConnected: true },
    };

    beforeEach(() => {
        vi.clearAllMocks();
        mockUseGameStore.mockReturnValue(defaultStore);
    });

    describe("引き分け提案ボタン", () => {
        it("通常のゲーム中は引き分け提案ボタンが表示される", () => {
            render(<DrawOfferDialog />);
            expect(screen.getByText("引き分け提案")).toBeInTheDocument();
        });

        it("引き分け提案中はボタンが無効化される", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                drawOfferPending: true,
                pendingDrawOfferer: "black", // 自分が提案者
            });
            render(<DrawOfferDialog />);
            const button = screen.getByText("引き分け提案中...");
            expect(button).toBeDisabled();
        });

        it("ゲーム終了時は引き分け提案ボタンが表示されない", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                gameStatus: "black_win",
            });
            const { container } = render(<DrawOfferDialog />);
            expect(container.firstChild).toBeNull();
        });

        it("オンライン対戦で相手の手番の場合、ボタンが表示されない", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                isOnlineGame: true,
                localPlayer: "black",
                currentPlayer: "white",
            });
            const { container } = render(<DrawOfferDialog />);
            expect(container.firstChild).toBeNull();
        });

        it("オンライン対戦で接続が切断されている場合、ボタンが表示されない", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                isOnlineGame: true,
                connectionStatus: { isConnected: false },
            });
            const { container } = render(<DrawOfferDialog />);
            expect(container.firstChild).toBeNull();
        });

        it("引き分け提案ボタンをクリックすると、offerDrawが呼ばれる", () => {
            render(<DrawOfferDialog />);
            fireEvent.click(screen.getByText("引き分け提案"));
            expect(mockOfferDraw).toHaveBeenCalledTimes(1);
        });
    });

    describe("引き分け提案受信ダイアログ（オンライン対戦）", () => {
        it("オンライン対戦で相手から提案を受けた場合、ダイアログが表示される", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                isOnlineGame: true,
                drawOfferPending: true,
                pendingDrawOfferer: "white",
                localPlayer: "black",
            });
            render(<DrawOfferDialog />);
            expect(screen.getByText("引き分け提案")).toBeInTheDocument();
            expect(
                screen.getByText("後手から引き分けの提案がありました。承認しますか？"),
            ).toBeInTheDocument();
        });

        it("承認ボタンをクリックすると、acceptDrawOfferが呼ばれる", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                isOnlineGame: true,
                drawOfferPending: true,
                pendingDrawOfferer: "white",
                localPlayer: "black",
            });
            render(<DrawOfferDialog />);
            fireEvent.click(screen.getByText("承認"));
            expect(mockAcceptDrawOffer).toHaveBeenCalledTimes(1);
        });

        it("拒否ボタンをクリックすると、rejectDrawOfferが呼ばれる", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                isOnlineGame: true,
                drawOfferPending: true,
                pendingDrawOfferer: "white",
                localPlayer: "black",
            });
            render(<DrawOfferDialog />);
            fireEvent.click(screen.getByText("拒否"));
            expect(mockRejectDrawOffer).toHaveBeenCalledTimes(1);
        });
    });

    describe("引き分け提案受信ダイアログ（ローカル対戦）", () => {
        it("ローカル対戦で相手から提案を受けた場合、ダイアログが表示される", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                isOnlineGame: false,
                drawOfferPending: true,
                pendingDrawOfferer: "black",
                currentPlayer: "white",
            });
            render(<DrawOfferDialog />);
            expect(screen.getByText("引き分け提案")).toBeInTheDocument();
            expect(
                screen.getByText("先手から引き分けの提案がありました。承認しますか？"),
            ).toBeInTheDocument();
        });

        it("自分が提案者の場合、ダイアログは表示されない", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                isOnlineGame: false,
                drawOfferPending: true,
                pendingDrawOfferer: "black",
                currentPlayer: "black",
            });
            render(<DrawOfferDialog />);
            expect(screen.getByText("引き分け提案中...")).toBeInTheDocument();
            expect(
                screen.queryByText("先手から引き分けの提案がありました。承認しますか？"),
            ).not.toBeInTheDocument();
        });
    });

    describe("エッジケース", () => {
        it("localPlayerがnullの場合、受信ダイアログは表示されない", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                isOnlineGame: true,
                drawOfferPending: true,
                pendingDrawOfferer: "white",
                localPlayer: null,
            });
            render(<DrawOfferDialog />);
            expect(
                screen.queryByText("後手から引き分けの提案がありました。承認しますか？"),
            ).not.toBeInTheDocument();
        });

        it("王手中でも引き分け提案が可能", () => {
            mockUseGameStore.mockReturnValue({
                ...defaultStore,
                gameStatus: "check",
            });
            render(<DrawOfferDialog />);
            expect(screen.getByText("引き分け提案")).toBeInTheDocument();
        });
    });
});
