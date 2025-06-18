import { useGameStore } from "@/stores/gameStore";
import { useTimerStore } from "@/stores/timerStore";
import { minutesToMs, secondsToMs } from "@/types/timer";
import { useEffect, useRef } from "react";
import { useGameSettings } from "./useGameSettings";

interface UseIntegratedTimerOptions {
    /** Callback when a player times out */
    onTimeout?: (player: "black" | "white") => void;
}

/**
 * Integrates the timer with game settings and game state
 * Automatically starts timer when game begins and switches on moves
 */
export function useIntegratedTimer({ onTimeout }: UseIntegratedTimerOptions = {}) {
    const { settings } = useGameSettings();
    const gameStore = useGameStore();
    const timerStore = useTimerStore();

    const previousMoveCount = useRef(gameStore.moveHistory.length);
    const previousGameStatus = useRef(gameStore.gameStatus);

    // Update timer config when settings change
    useEffect(() => {
        const { timeControl } = settings;
        useTimerStore.getState().updateConfig({
            enabled: timeControl.enabled,
            mainTimeMs: minutesToMs(timeControl.mainTimeMinutes),
            byoyomiMs: secondsToMs(timeControl.byoyomiSeconds),
        });
    }, [settings]);

    // Handle game state changes
    useEffect(() => {
        const currentMoveCount = gameStore.moveHistory.length;
        const currentGameStatus = gameStore.gameStatus;
        const wasGameStarted = previousMoveCount.current === 0 && currentMoveCount === 1;
        const didMakeMove = currentMoveCount > previousMoveCount.current;
        const isGameEnded = currentGameStatus !== "playing" && currentGameStatus !== "check";
        const wasGameReset = currentMoveCount === 0 && previousMoveCount.current > 0;

        // Start timer when first move is made
        if (wasGameStarted && settings.timeControl.enabled) {
            useTimerStore.getState().start("black"); // Black moves first
        }

        // Switch timer when a move is made (but not the first move)
        else if (didMakeMove && currentMoveCount > 1 && settings.timeControl.enabled) {
            useTimerStore.getState().switchPlayer();
        }

        // Pause timer when game ends
        else if (isGameEnded) {
            useTimerStore.getState().pause();
        }

        // Reset timer when game is reset
        else if (wasGameReset) {
            useTimerStore.getState().reset();
        }

        // Update refs
        previousMoveCount.current = currentMoveCount;
        previousGameStatus.current = currentGameStatus;
    }, [gameStore.moveHistory.length, gameStore.gameStatus, settings.timeControl.enabled]);

    // Handle timeout
    useEffect(() => {
        if (timerStore.hasTimedOut && timerStore.timedOutPlayer) {
            // Call the timeout callback
            onTimeout?.(timerStore.timedOutPlayer);

            // Update game status to indicate timeout - resign will end the game
            gameStore.resign(); // This will end the game
        }
    }, [timerStore.hasTimedOut, timerStore.timedOutPlayer, onTimeout, gameStore]);

    // Reset timer when game is reset via game store
    useEffect(() => {
        if (gameStore.moveHistory.length === 0 && previousMoveCount.current > 0) {
            useTimerStore.getState().reset();
        }
    }, [gameStore.moveHistory.length]);

    return {
        // Timer state
        blackMainTime: timerStore.blackMainTime,
        whiteMainTime: timerStore.whiteMainTime,
        byoyomiTime: timerStore.byoyomiTime,
        isRunning: timerStore.isRunning,
        activePlayer: timerStore.activePlayer,
        hasTimedOut: timerStore.hasTimedOut,
        timedOutPlayer: timerStore.timedOutPlayer,

        // Timer config
        config: timerStore.config,

        // Manual controls (for testing or special situations)
        manualStart: timerStore.start,
        manualPause: timerStore.pause,
        manualResume: timerStore.resume,
        manualReset: timerStore.reset,
        manualSwitchPlayer: timerStore.switchPlayer,
    };
}
