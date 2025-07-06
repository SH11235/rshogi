import * as useOpeningBookModule from "@/hooks/useOpeningBook";
import { act, fireEvent, render, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { OpeningBook } from "../OpeningBook";

vi.mock("@/stores/gameStore");

describe("OpeningBook", () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });
    it("should render title", async () => {
        // Arrange & Act
        const { getByText } = render(<OpeningBook />);

        // Assert
        await waitFor(() => {
            expect(getByText("定跡")).toBeInTheDocument();
        });
    });

    it("should display book moves", async () => {
        // Arrange
        vi.spyOn(useOpeningBookModule, "useOpeningBook").mockReturnValue({
            moves: [
                { notation: "7g7f", evaluation: 50, depth: 10 },
                { notation: "2g2f", evaluation: 45, depth: 8 },
            ],
            loading: false,
            error: null,
            level: "early",
            progress: null,
            loadMoreData: vi.fn(),
        });

        // Act
        const { getByText } = render(<OpeningBook />);

        // Assert
        await waitFor(() => {
            expect(getByText("7g7f")).toBeInTheDocument();
            expect(getByText("評価: +50 深さ: 10")).toBeInTheDocument();
        });
    });

    it("should display error message", async () => {
        // Arrange
        vi.spyOn(useOpeningBookModule, "useOpeningBook").mockReturnValue({
            moves: [],
            loading: false,
            error: "定跡データの読み込みに失敗しました",
            level: "early",
            progress: null,
            loadMoreData: vi.fn(),
        });

        // Act
        const { getByText } = render(<OpeningBook />);

        // Assert
        await waitFor(() => {
            expect(getByText("エラー: 定跡データの読み込みに失敗しました")).toBeInTheDocument();
        });
    });

    it("should display data level selector", async () => {
        // Arrange
        vi.spyOn(useOpeningBookModule, "useOpeningBook").mockReturnValue({
            moves: [],
            loading: false,
            error: null,
            level: "early",
            progress: null,
            loadMoreData: vi.fn(),
        });

        // Act
        const { getByText } = render(<OpeningBook />);

        // Assert
        await waitFor(() => {
            expect(getByText("序盤")).toBeInTheDocument();
            expect(getByText("標準")).toBeInTheDocument();
            expect(getByText("完全版")).toBeInTheDocument();
        });
    });

    it("should call loadMoreData when level button clicked", async () => {
        // Arrange
        const loadMoreData = vi.fn();
        vi.spyOn(useOpeningBookModule, "useOpeningBook").mockReturnValue({
            moves: [],
            loading: false,
            error: null,
            level: "early",
            progress: null,
            loadMoreData,
        });

        // Act
        const { getByText } = render(<OpeningBook />);

        await act(async () => {
            fireEvent.click(getByText("標準"));
        });

        // Assert
        await waitFor(() => {
            expect(loadMoreData).toHaveBeenCalledWith("standard");
        });
    });
});
