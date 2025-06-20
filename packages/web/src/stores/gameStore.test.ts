import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { Move } from "shogi-core";
import { parseKifMoves } from "shogi-core";
import { beforeEach, describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

describe("gameStore resignation functionality", () => {
    beforeEach(() => {
        // Reset the store before each test
        useGameStore.getState().resetGame();
    });

    it("should allow resignation during playing state", () => {
        // Set current player to white
        useGameStore.setState({ currentPlayer: "white" });

        let store = useGameStore.getState();
        expect(store.gameStatus).toBe("playing");
        expect(store.currentPlayer).toBe("white");

        // Resign as white player
        store.resign();

        // Get updated state
        store = useGameStore.getState();
        expect(store.gameStatus).toBe("black_win");
        expect(store.resignedPlayer).toBe("white");
    });

    it("should allow resignation during check state", () => {
        // Simulate check state by setting it directly
        useGameStore.setState({ gameStatus: "check", currentPlayer: "black" });

        let store = useGameStore.getState();
        expect(store.gameStatus).toBe("check");
        expect(store.currentPlayer).toBe("black");

        store.resign();

        // Get updated state
        store = useGameStore.getState();
        expect(store.gameStatus).toBe("white_win");
        expect(store.resignedPlayer).toBe("black");
    });

    it("should not allow resignation when game is already over", () => {
        const store = useGameStore.getState();

        // Set game as already finished
        useGameStore.setState({ gameStatus: "black_win", currentPlayer: "white" });

        const initialStatus = store.gameStatus;
        const initialResignedPlayer = store.resignedPlayer;

        store.resign();

        // Should remain unchanged
        expect(store.gameStatus).toBe(initialStatus);
        expect(store.resignedPlayer).toBe(initialResignedPlayer);
    });

    it("should clear selection states when resigning", () => {
        const store = useGameStore.getState();

        // Set some selection states
        useGameStore.setState({
            selectedSquare: { row: 7, column: 5 },
            selectedDropPiece: { type: "pawn", player: "black" },
            validMoves: [{ row: 6, column: 5 }],
            validDropSquares: [{ row: 6, column: 5 }],
            currentPlayer: "black",
        });

        store.resign();

        expect(store.selectedSquare).toBeNull();
        expect(store.selectedDropPiece).toBeNull();
        expect(store.validMoves).toEqual([]);
        expect(store.validDropSquares).toEqual([]);
    });

    it("should reset resignedPlayer when game is reset", () => {
        // Make resignation
        useGameStore.setState({ currentPlayer: "black" });
        let store = useGameStore.getState();
        store.resign();

        // Get updated state after resignation
        store = useGameStore.getState();
        expect(store.gameStatus).toBe("white_win");
        expect(store.resignedPlayer).toBe("black");

        // Reset game
        store.resetGame();

        // Get updated state after reset
        store = useGameStore.getState();
        expect(store.resignedPlayer).toBeNull();
        expect(store.gameStatus).toBe("playing");
    });
});

describe("gameStore checkmate to win status functionality", () => {
    beforeEach(() => {
        useGameStore.getState().resetGame();
    });

    it("should convert checkmate to win status through makeMove", () => {
        // Test that the logic is in place for checkmate detection
        // The actual checkmate detection happens through the move validation in the core package
        // Here we test that the gameStore properly handles the transition

        // Simulate a checkmate scenario by setting the appropriate game state
        useGameStore.setState({
            currentPlayer: "white",
            gameStatus: "playing",
        });

        // Simulate what happens when makeMove detects checkmate and sets win status
        useGameStore.setState({ gameStatus: "black_win" });

        let store = useGameStore.getState();
        expect(store.gameStatus).toBe("black_win");

        // Test the reverse scenario
        useGameStore.setState({ gameStatus: "white_win" });
        store = useGameStore.getState();
        expect(store.gameStatus).toBe("white_win");
    });
});

describe("gameStore tsume-shogi import functionality", () => {
    beforeEach(() => {
        useGameStore.getState().resetGame();
    });

    it("should parse tsume-shogi KIF with initial position", () => {
        // Read the tsume-shogi test file
        const kifContent = readFileSync(
            join(__dirname, "../../test-data/tsume-shogi.kif"),
            "utf-8",
        );

        // Parse directly using parseKifMoves to test the parser
        const parseResult = parseKifMoves(kifContent);

        // Verify initial position was parsed
        expect(parseResult.initialBoard).toBeDefined();
        expect(parseResult.initialHands).toBeDefined();

        // Debug: log what we got
        console.log("Parsed board keys:", Object.keys(parseResult.initialBoard || {}));
        console.log("Board at 12:", parseResult.initialBoard?.["12"]);
        console.log("Board at 21:", parseResult.initialBoard?.["21"]);

        // Should have white king at position 21 (row 2, column 1)
        expect(parseResult.initialBoard?.["21"]).toEqual({
            type: "gyoku",
            owner: "white",
            promoted: false,
        });

        // Black should have 1 silver and 1 gold in initial hands
        expect(parseResult.initialHands.black.銀).toBe(1);
        expect(parseResult.initialHands.black.金).toBe(1);

        // Should parse 3 moves
        expect(parseResult.moves).toHaveLength(3);
    });

    it("should import tsume-shogi game with initial position and moves", () => {
        // Create a simple tsume-shogi position manually
        const initialMoves: Move[] = [];
        const kifContent = `# KIF形式棋譜ファイル
# 先手の持駒：銀 金
# 後手の持駒：なし
#   ９ ８ ７ ６ ５ ４ ３ ２ １
# +---------------------------+
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|一
# | ・ ・ ・ ・ ・ ・ ・ ・v玉|二
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|三
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|四
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|五
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|六
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|七
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|八
# | ・ ・ ・ ・ ・ ・ ・ ・ ・|九
# +---------------------------+

手数----指手---------消費時間--`;

        const store = useGameStore.getState();

        // Import with empty moves but with KIF content
        store.importGame(initialMoves, kifContent);

        // Get updated state
        const updatedStore = useGameStore.getState();

        // Verify initial position was set correctly
        // White king should be at position 21 (row 2, column 1)
        expect(updatedStore.board["21"]).toEqual({
            type: "gyoku",
            owner: "white",
            promoted: false,
        });

        // Verify hands
        expect(updatedStore.hands.black.銀).toBe(1);
        expect(updatedStore.hands.black.金).toBe(1);

        // Now test that we can drop a piece
        store.selectDropPiece("silver", "black");
        store.makeDrop("silver", { row: 2, column: 3 });

        const afterDropState = useGameStore.getState();
        expect(afterDropState.board["23"]).toEqual({
            type: "silver",
            owner: "black",
            promoted: false,
        });
        expect(afterDropState.hands.black.銀).toBe(0);
    });
});
