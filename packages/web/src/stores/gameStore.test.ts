import type { Move } from "shogi-core";
import { beforeEach, describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

describe("gameStore - KIF import and start from position", () => {
    beforeEach(() => {
        // Reset store before each test
        useGameStore.setState({
            gameType: "ai", // Start with AI mode to test the bug
            gameMode: "playing",
            isAIThinking: false,
        });
    });

    it("should reset gameType to local when importing KIF", () => {
        const store = useGameStore.getState();
        const moves: Move[] = [
            {
                type: "normal",
                from: { row: 2, column: 7 },
                to: { row: 2, column: 6 },
                piece: { type: "pawn", owner: "black" },
            },
        ];

        // Import KIF file
        store.importGame(moves, "# KIF test");

        const state = useGameStore.getState();
        expect(state.gameType).toBe("local");
        expect(state.gameMode).toBe("review");
        expect(state.aiPlayer).toBeNull();
        expect(state.aiPlayerInfo).toBeNull();
        expect(state.isAIThinking).toBe(false);
    });

    it("should set gameType to local when starting from position", () => {
        const store = useGameStore.getState();
        const moves: Move[] = [
            {
                type: "normal",
                from: { row: 2, column: 7 },
                to: { row: 2, column: 6 },
                piece: { type: "pawn", owner: "black" },
            },
        ];

        // First import KIF file
        store.importGame(moves, "# KIF test");

        // Then start game from position
        store.startGameFromPosition();

        const state = useGameStore.getState();
        expect(state.gameType).toBe("local");
        expect(state.gameMode).toBe("playing");
    });

    it("should allow moves after starting from position", () => {
        const store = useGameStore.getState();
        const moves: Move[] = [];

        // Set up AI mode initially
        useGameStore.setState({
            gameType: "ai",
            localPlayerColor: "white", // Player is white, so black moves would be blocked
        });

        // Import KIF and start from position
        store.importGame(moves, "# KIF test");
        store.startGameFromPosition();

        // Try to select a square with a black pawn (black's turn)
        store.selectSquare({ row: 7, column: 7 });

        const state = useGameStore.getState();
        // Should be able to select square because gameType is now "local"
        expect(state.selectedSquare).toEqual({ row: 7, column: 7 });
    });
});
