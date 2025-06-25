import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { P2PTestPage } from "./P2PTestPage";

// Mock the P2PTestPanel component
vi.mock("../components/P2PTestPanel", () => ({
    P2PTestPanel: () => <div data-testid="p2p-test-panel">Mocked P2P Test Panel</div>,
}));

describe("P2PTestPage", () => {
    it("should render the page title", () => {
        render(<P2PTestPage />);
        expect(screen.getByText("Shogi P2P Test")).toBeInTheDocument();
    });

    it("should render with correct layout", () => {
        render(<P2PTestPage />);

        const container = screen.getByText("Shogi P2P Test").parentElement;
        expect(container).toHaveClass("container", "mx-auto", "px-4");

        const wrapper = container?.parentElement;
        expect(wrapper).toHaveClass("min-h-screen", "bg-gray-100", "py-8");
    });

    it("should render the P2PTestPanel component", () => {
        render(<P2PTestPage />);
        expect(screen.getByTestId("p2p-test-panel")).toBeInTheDocument();
    });

    it("should have proper heading styling", () => {
        render(<P2PTestPage />);
        const heading = screen.getByText("Shogi P2P Test");
        expect(heading.tagName).toBe("H1");
        expect(heading).toHaveClass("text-3xl", "font-bold", "mb-4");
    });
});
