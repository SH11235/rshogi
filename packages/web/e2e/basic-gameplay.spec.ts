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

        // Target desktop layout to avoid duplicate element issues
        const gameInfoSection = shogiBoardPage.page.locator(".hidden.lg\\:flex").first();

        // Verify initial turn
        await expect(gameInfoSection.getByText("先手番")).toBeVisible();

        // Make a basic move: black pawn 7-7 to 7-6
        await shogiBoardPage.makeMove(7, 7, 6, 7);

        // Verify turn changed to white
        await expect(gameInfoSection.getByText("後手番")).toBeVisible();

        // Verify move was recorded in history
        await expect(gameInfoSection.getByRole("button", { name: /☗1\. 歩/ })).toBeVisible();

        // Test undo functionality
        await shogiBoardPage.undo();

        // Verify return to initial state
        await expect(gameInfoSection.getByText("先手番")).toBeVisible();
        await expect(gameInfoSection.getByText("第1手")).toBeVisible();
    });

    test("Failure Case: Invalid move attempt", async () => {
        const gameInfoSection = shogiBoardPage.page.locator(".hidden.lg\\:flex").first();

        // Try to click opponent's piece (should fail silently)
        await shogiBoardPage.clickSquare(3, 3);

        // Verify no state change occurred - still on first turn
        await expect(gameInfoSection.getByText("第1手")).toBeVisible();

        // Try to click empty square without selecting a piece first
        await shogiBoardPage.clickSquare(5, 5);

        // Verify still at initial state
        await expect(gameInfoSection.getByText("第1手")).toBeVisible();
    });
});
