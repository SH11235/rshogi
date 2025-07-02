import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useGameStore } from "../stores/gameStore";
import type { MateSearchState } from "../types/mateSearch";
import { MateSearchPanel } from "./MateSearchPanel";

// gameStoreをモック
vi.mock("../stores/gameStore");

const mockedUseGameStore = vi.mocked(useGameStore);

describe("MateSearchPanel", () => {
    const mockStartMateSearch = vi.fn();
    const mockCancelMateSearch = vi.fn();

    const defaultMateSearchState: MateSearchState = {
        status: "idle",
        depth: 0,
        maxDepth: 7,
        result: null,
        error: null,
    };

    beforeEach(() => {
        vi.clearAllMocks();
        mockedUseGameStore.mockReturnValue({
            mateSearch: defaultMateSearchState,
            startMateSearch: mockStartMateSearch,
            cancelMateSearch: mockCancelMateSearch,
            gameStatus: "playing",
        });
    });

    it("詰み探索パネルが表示される", () => {
        render(<MateSearchPanel />);
        expect(screen.getByText("詰み探索")).toBeInTheDocument();
        expect(screen.getByText("詰み探索を開始")).toBeInTheDocument();
    });

    it("探索深さを選択できる", () => {
        render(<MateSearchPanel />);
        const select = screen.getByLabelText("探索深さ:");
        expect(select).toHaveValue("7");

        fireEvent.change(select, { target: { value: "3" } });
        expect(select).toHaveValue("3");
    });

    it("探索を開始できる", async () => {
        render(<MateSearchPanel />);
        const button = screen.getByRole("button", { name: /詰み探索を開始/ });

        fireEvent.click(button);

        await waitFor(() => {
            expect(mockStartMateSearch).toHaveBeenCalledWith({ maxDepth: 7 });
        });
    });

    it("探索中はキャンセルボタンが表示される", () => {
        mockedUseGameStore.mockReturnValue({
            mateSearch: { ...defaultMateSearchState, status: "searching" },
            startMateSearch: mockStartMateSearch,
            cancelMateSearch: mockCancelMateSearch,
            gameStatus: "playing",
        });

        render(<MateSearchPanel />);
        expect(screen.getByText("キャンセル")).toBeInTheDocument();
    });

    it("詰みが見つかった場合の表示", () => {
        mockedUseGameStore.mockReturnValue({
            mateSearch: {
                ...defaultMateSearchState,
                status: "found",
                result: {
                    isMate: true,
                    moves: ["76→75", "34→35"],
                    nodeCount: 1234,
                    elapsedMs: 100,
                    depth: 2,
                },
            },
            startMateSearch: mockStartMateSearch,
            cancelMateSearch: mockCancelMateSearch,
            gameStatus: "playing",
        });

        render(<MateSearchPanel />);
        expect(screen.getByText("2手詰め発見")).toBeInTheDocument();
        expect(screen.getByText("1. 76→75")).toBeInTheDocument();
        expect(screen.getByText("2. 34→35")).toBeInTheDocument();
        expect(screen.getByText(/探索ノード数: 1,234/)).toBeInTheDocument();
        expect(screen.getByText(/実行時間: 100ms/)).toBeInTheDocument();
    });

    it("詰みが見つからなかった場合の表示", () => {
        mockedUseGameStore.mockReturnValue({
            mateSearch: {
                ...defaultMateSearchState,
                status: "not_found",
                maxDepth: 5,
                result: {
                    isMate: false,
                    moves: [],
                    nodeCount: 5678,
                    elapsedMs: 200,
                    depth: 0,
                },
            },
            startMateSearch: mockStartMateSearch,
            cancelMateSearch: mockCancelMateSearch,
            gameStatus: "playing",
        });

        render(<MateSearchPanel />);
        expect(screen.getByText("5手以内に詰みなし")).toBeInTheDocument();
    });

    it("ゲーム終了状態では探索ボタンが無効", () => {
        mockedUseGameStore.mockReturnValue({
            mateSearch: defaultMateSearchState,
            startMateSearch: mockStartMateSearch,
            cancelMateSearch: mockCancelMateSearch,
            gameStatus: "checkmate",
        });

        render(<MateSearchPanel />);
        const button = screen.getByRole("button", { name: /詰み探索を開始/ });
        expect(button).toBeDisabled();
    });

    it("エラーが発生した場合の表示", () => {
        mockedUseGameStore.mockReturnValue({
            mateSearch: {
                ...defaultMateSearchState,
                status: "error",
                error: "探索中にエラーが発生しました",
            },
            startMateSearch: mockStartMateSearch,
            cancelMateSearch: mockCancelMateSearch,
            gameStatus: "playing",
        });

        render(<MateSearchPanel />);
        expect(screen.getByText("エラー: 探索中にエラーが発生しました")).toBeInTheDocument();
    });
});
