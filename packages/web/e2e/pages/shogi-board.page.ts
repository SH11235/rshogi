import { type Page, expect } from "@playwright/test";

export class ShogiBoardPage {
    readonly page: Page;

    constructor(page: Page) {
        this.page = page;
    }

    async goto() {
        await this.page.goto("/");
        await expect(this.page.getByText("将棋")).toBeVisible();
    }

    async getSquare(row: number, col: number) {
        // Convert to Japanese notation (9-1 from left to right, 一-九 from top to bottom)
        const colNumber = 10 - col; // Convert 1-9 to 9-1
        const kanjiNumbers = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];
        const rowKanji = kanjiNumbers[row - 1];

        return this.page.getByRole("button", {
            name: new RegExp(`Square ${colNumber}${rowKanji}`),
        });
    }

    async clickSquare(row: number, col: number) {
        const square = await this.getSquare(row, col);
        await square.click();
    }

    async makeMove(fromRow: number, fromCol: number, toRow: number, toCol: number) {
        await this.clickSquare(fromRow, fromCol);
        await this.clickSquare(toRow, toCol);
    }

    async expectMoveInHistory(moveNotation: string) {
        await expect(this.page.getByText(new RegExp(moveNotation))).toBeVisible();
    }

    async undo() {
        // Look for the undo button (now "戻す" in playing mode)
        await this.page.getByRole("button", { name: "戻す" }).click();
    }

    async redo() {
        // Look for the redo button (now "進む" in playing mode)
        const redoButton = this.page.getByRole("button", { name: "進む" });

        // Wait for the button to be enabled
        await expect(redoButton).toBeEnabled();
        await redoButton.click();
    }
}
