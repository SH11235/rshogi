import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { HandPiecesDisplay } from "./HandPiecesDisplay";

describe("HandPiecesDisplay", () => {
    const defaultHand = {
        R: 0,
        B: 0,
        G: 0,
        S: 0,
        N: 0,
        L: 0,
        P: 0,
    };

    const mockHandSelect = vi.fn();

    describe("レイアウトロジック", () => {
        it("normal サイズでは count=0 でも全駒種を表示する", () => {
            render(
                <HandPiecesDisplay
                    owner="sente"
                    hand={defaultHand}
                    selectedPiece={null}
                    isActive={false}
                    onHandSelect={mockHandSelect}
                    size="normal"
                />,
            );

            // 全7種類の駒が表示される（飛、角、金、銀、桂、香、歩）
            // getByText は要素が見つからない場合にエラーをスローする
            expect(screen.getByText("飛")).toBeDefined();
            expect(screen.getByText("角")).toBeDefined();
            expect(screen.getByText("金")).toBeDefined();
            expect(screen.getByText("銀")).toBeDefined();
            expect(screen.getByText("桂")).toBeDefined();
            expect(screen.getByText("香")).toBeDefined();
            expect(screen.getByText("歩")).toBeDefined();
        });

        it("compact サイズでは count=0 の駒は表示しない（対局中）", () => {
            render(
                <HandPiecesDisplay
                    owner="sente"
                    hand={{ ...defaultHand, P: 3, G: 1 }}
                    selectedPiece={null}
                    isActive={false}
                    onHandSelect={mockHandSelect}
                    size="compact"
                    isMatchRunning={true}
                />,
            );

            // 持っている駒のみ表示
            expect(screen.getByText("金")).toBeDefined();
            expect(screen.getByText("歩")).toBeDefined();

            // 持っていない駒は表示されない
            expect(screen.queryByText("飛")).toBeNull();
            expect(screen.queryByText("角")).toBeNull();
            expect(screen.queryByText("銀")).toBeNull();
            expect(screen.queryByText("桂")).toBeNull();
            expect(screen.queryByText("香")).toBeNull();
        });

        it("medium サイズでも count=0 の駒は表示しない（対局中）", () => {
            render(
                <HandPiecesDisplay
                    owner="gote"
                    hand={{ ...defaultHand, R: 1 }}
                    selectedPiece={null}
                    isActive={false}
                    onHandSelect={mockHandSelect}
                    size="medium"
                    isMatchRunning={true}
                />,
            );

            // 持っている飛車のみ表示
            expect(screen.getByText("飛")).toBeDefined();

            // 他は表示されない
            expect(screen.queryByText("角")).toBeNull();
            expect(screen.queryByText("金")).toBeNull();
        });

        it("compact サイズでも編集モードでは全駒種を表示する", () => {
            render(
                <HandPiecesDisplay
                    owner="sente"
                    hand={defaultHand}
                    selectedPiece={null}
                    isActive={false}
                    onHandSelect={mockHandSelect}
                    size="compact"
                    isEditMode={true}
                />,
            );

            // 編集モードでは全7種類の駒が表示される
            expect(screen.getByText("飛")).toBeDefined();
            expect(screen.getByText("角")).toBeDefined();
            expect(screen.getByText("金")).toBeDefined();
            expect(screen.getByText("銀")).toBeDefined();
            expect(screen.getByText("桂")).toBeDefined();
            expect(screen.getByText("香")).toBeDefined();
            expect(screen.getByText("歩")).toBeDefined();
        });
    });

    describe("プレイヤーマーカー", () => {
        it("先手の場合は ☗ マーカーを表示", () => {
            render(
                <HandPiecesDisplay
                    owner="sente"
                    hand={defaultHand}
                    selectedPiece={null}
                    isActive={false}
                    onHandSelect={mockHandSelect}
                />,
            );

            expect(screen.getByText("☗")).toBeDefined();
        });

        it("後手の場合は ☖ マーカーを表示", () => {
            render(
                <HandPiecesDisplay
                    owner="gote"
                    hand={defaultHand}
                    selectedPiece={null}
                    isActive={false}
                    onHandSelect={mockHandSelect}
                />,
            );

            expect(screen.getByText("☖")).toBeDefined();
        });
    });

    describe("±ボタン表示", () => {
        it("normal サイズかつ編集モードでは ± ボタンが表示される", () => {
            render(
                <HandPiecesDisplay
                    owner="sente"
                    hand={{ ...defaultHand, P: 2 }}
                    selectedPiece={null}
                    isActive={false}
                    onHandSelect={mockHandSelect}
                    size="normal"
                    isEditMode={true}
                    onIncrement={vi.fn()}
                    onDecrement={vi.fn()}
                />,
            );

            // + と − ボタンが存在する（複数あるので getAllByText を使用）
            const plusButtons = screen.getAllByText("+");
            const minusButtons = screen.getAllByText("−");

            expect(plusButtons.length).toBeGreaterThan(0);
            expect(minusButtons.length).toBeGreaterThan(0);
        });

        it("compact サイズでも編集モードでは ± ボタンが表示される", () => {
            render(
                <HandPiecesDisplay
                    owner="sente"
                    hand={{ ...defaultHand, P: 2 }}
                    selectedPiece={null}
                    isActive={false}
                    onHandSelect={mockHandSelect}
                    size="compact"
                    isEditMode={true}
                    onIncrement={vi.fn()}
                    onDecrement={vi.fn()}
                />,
            );

            const plusButtons = screen.getAllByText("+");
            expect(plusButtons.length).toBeGreaterThan(0);
        });
    });
});
