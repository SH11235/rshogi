import { expect, test } from "@playwright/test";
import { ShogiBoardPage } from "./pages/shogi-board.page";

test.describe("Shogi Core Functionality", () => {
    let shogiBoardPage: ShogiBoardPage;

    test.beforeEach(async ({ page }) => {
        // Set consistent viewport for stable testing - desktop mode
        await page.setViewportSize({ width: 1280, height: 720 });
        shogiBoardPage = new ShogiBoardPage(page);
        await shogiBoardPage.goto();
        // Wait for page to be fully loaded
        await page.waitForLoadState("networkidle");
    });

    test("Happy Path: Basic game flow", async () => {
        // Verify initial state - count only board square buttons
        const squares = await shogiBoardPage.page.getByRole("button", { name: /Square/ }).all();
        expect(squares).toHaveLength(81);

        // Make a basic move: black pawn 7-7 to 7-6
        await shogiBoardPage.makeMove(7, 7, 6, 7);

        // Verify move was recorded in history
        await expect(shogiBoardPage.page.getByRole("button", { name: /☗1\. 歩/ })).toBeVisible();

        // Test undo functionality
        await shogiBoardPage.undo();

        // Verify we can make the same move again after undo
        await shogiBoardPage.makeMove(7, 7, 6, 7);
        await expect(shogiBoardPage.page.getByRole("button", { name: /☗1\. 歩/ })).toBeVisible();
    });

    test("Failure Case: Invalid move attempt", async () => {
        // Try to click opponent's piece (should fail silently)
        await shogiBoardPage.clickSquare(3, 3);

        // Try to click empty square without selecting a piece first
        await shogiBoardPage.clickSquare(5, 5);

        // Verify no moves were recorded - history should be empty
        const historyButtons = await shogiBoardPage.page
            .getByRole("button", { name: /☗\d+\./ })
            .all();
        expect(historyButtons).toHaveLength(0);
    });
});
