import { audioManager } from "@/services/audioManager";
import type { SoundType } from "@/types/audio";
import {
    type TimerActions,
    type TimerConfig,
    type TimerState,
    getWarningLevel,
} from "@/types/timer";
import {
    type Board,
    type GameStatus,
    HISTORY_CURSOR,
    type Hands,
    type Move,
    type Piece,
    type PieceType,
    type Player,
    type Square,
    applyMove,
    canPromoteByPosition,
    generateLegalDropMovesForPiece,
    generateLegalMoves,
    initialHands,
    isCheckmate,
    isInCheck,
    modernInitialBoard,
    mustPromote,
    parseKifMoves,
    parseSfen,
    reconstructGameState,
} from "shogi-core";
import { create } from "zustand";

// 音声再生ヘルパー関数
function playGameSound(soundType: SoundType): void {
    audioManager.play(soundType).catch((error) => {
        console.debug(`Failed to play ${soundType} sound:`, error);
    });
}

// ドロップ可能位置を計算（王手放置チェック含む）
function getValidDropSquares(
    board: Board,
    hands: Hands,
    pieceType: PieceType,
    player: Player,
): Square[] {
    // 合法手生成機能を使用
    return generateLegalDropMovesForPiece(board, hands, pieceType, player);
}

interface PromotionPendingMove {
    from: Square;
    to: Square;
    piece: Piece;
}

interface GameState {
    board: Board;
    hands: Hands;
    currentPlayer: Player;
    selectedSquare: Square | null;
    selectedDropPiece: { type: PieceType; player: Player } | null;
    validMoves: Square[];
    validDropSquares: Square[];
    moveHistory: Move[];
    historyCursor: number; // 現在表示している履歴位置
    gameStatus: GameStatus;
    promotionPending: PromotionPendingMove | null;
    resignedPlayer: Player | null; // 投了したプレイヤー

    // タイマー状態
    timer: TimerState;

    selectSquare: (square: Square) => void;
    selectDropPiece: (pieceType: PieceType, player: Player) => void;
    makeMove: (from: Square, to: Square, promote?: boolean) => void;
    makeDrop: (pieceType: PieceType, to: Square) => void;
    confirmPromotion: (promote: boolean) => void;
    cancelPromotion: () => void;
    clearSelections: () => void;
    resetGame: () => void;
    resign: () => void;
    importGame: (moves: Move[], kifContent?: string) => void;
    importSfen: (sfen: string) => void;
    // 履歴操作機能
    undo: () => void;
    redo: () => void;
    goToMove: (moveIndex: number) => void;
    canUndo: () => boolean;
    canRedo: () => boolean;
}

// タイマーアクションをGameStateに統合
interface GameState extends TimerActions {}

// タイマーの初期状態
function createInitialTimerState(): TimerState {
    return {
        config: {
            mode: null,
            basicTime: 0,
            byoyomiTime: 0,
            fischerIncrement: 0,
            perMoveLimit: 0,
        },
        blackTime: 0,
        whiteTime: 0,
        blackInByoyomi: false,
        whiteInByoyomi: false,
        activePlayer: null,
        isPaused: false,
        lastTickTime: 0,
        blackWarningLevel: "normal",
        whiteWarningLevel: "normal",
        hasTimedOut: false,
        timedOutPlayer: null,
    };
}

export const useGameStore = create<GameState>((set, get) => ({
    board: modernInitialBoard,
    hands: structuredClone(initialHands()),
    currentPlayer: "black",
    selectedSquare: null,
    selectedDropPiece: null,
    validMoves: [],
    validDropSquares: [],
    moveHistory: [],
    historyCursor: HISTORY_CURSOR.LATEST_POSITION, // 最新の状態を示す
    gameStatus: "playing",
    promotionPending: null,
    resignedPlayer: null,
    timer: createInitialTimerState(),

    selectSquare: (square: Square) => {
        const {
            board,
            currentPlayer,
            selectedSquare,
            selectedDropPiece,
            validDropSquares,
            gameStatus,
        } = get();

        // ゲーム終了時は操作不可
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        const squareKey = `${square.row}${square.column}` as keyof Board;
        const piece = board[squareKey];

        // ドロップモードの場合
        if (selectedDropPiece) {
            const isValidDrop = validDropSquares.some(
                (s) => s.row === square.row && s.column === square.column,
            );
            if (isValidDrop) {
                get().makeDrop(selectedDropPiece.type, square);
            } else {
                // 無効なドロップ先、選択解除
                set({ selectedDropPiece: null, validDropSquares: [] });
            }
            return;
        }

        // 既に選択されているマスをクリックした場合は選択解除
        if (selectedSquare?.row === square.row && selectedSquare?.column === square.column) {
            set({ selectedSquare: null, validMoves: [] });
            return;
        }

        // 自分の駒を選択
        if (piece && piece.owner === currentPlayer) {
            try {
                const { hands } = get();
                // 王手放置チェックを含む合法手のみを生成
                const validMoves = generateLegalMoves(board, hands, square, currentPlayer);
                set({
                    selectedSquare: square,
                    validMoves,
                    // ドロップモードをクリア
                    selectedDropPiece: null,
                    validDropSquares: [],
                });
                console.log(
                    `Selected piece at ${square.row}${square.column}, legal moves:`,
                    validMoves,
                );
            } catch (error) {
                console.error("Error generating legal moves:", error);
                set({ selectedSquare: null, validMoves: [] });
            }
        }
        // 移動先を選択
        else if (selectedSquare) {
            const { validMoves } = get();
            const isValidMove = validMoves.some(
                (m) => m.row === square.row && m.column === square.column,
            );
            if (isValidMove) {
                const fromKey = `${selectedSquare.row}${selectedSquare.column}` as keyof Board;
                const movingPiece = board[fromKey];

                if (movingPiece) {
                    // 成りを強制される場合は自動で成る
                    if (mustPromote(movingPiece, square)) {
                        console.log("Forced promotion");
                        get().makeMove(selectedSquare, square, true);
                    }
                    // 成り可能な場合はダイアログを表示
                    else if (canPromoteByPosition(movingPiece, selectedSquare, square)) {
                        console.log("Showing promotion dialog");
                        set({
                            promotionPending: {
                                from: selectedSquare,
                                to: square,
                                piece: movingPiece,
                            },
                            selectedSquare: null,
                            validMoves: [],
                        });
                    }
                    // 通常の移動
                    else {
                        console.log(
                            `Making move from ${selectedSquare.row}${selectedSquare.column} to ${square.row}${square.column}`,
                        );
                        get().makeMove(selectedSquare, square);
                    }
                }
            } else {
                // 無効な移動先を選択した場合、選択を解除
                set({ selectedSquare: null, validMoves: [] });
            }
        }
        // 何も選択していない状態で空のマス或いは相手の駒をクリック
        else {
            set({
                selectedSquare: null,
                validMoves: [],
                selectedDropPiece: null,
                validDropSquares: [],
            });
        }
    },

    selectDropPiece: (pieceType: PieceType, player: Player) => {
        const { board, hands, currentPlayer, gameStatus } = get();

        // ゲーム終了時は操作不可
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        // 手番プレイヤーでない場合は無視
        if (player !== currentPlayer) {
            return;
        }

        // 同じ駒を再度選択した場合は選択解除
        const { selectedDropPiece } = get();
        if (selectedDropPiece?.type === pieceType && selectedDropPiece?.player === player) {
            set({ selectedDropPiece: null, validDropSquares: [] });
            return;
        }

        const validDropSquares = getValidDropSquares(board, hands, pieceType, player);
        set({
            selectedDropPiece: { type: pieceType, player },
            validDropSquares,
            // 通常の移動モードをクリア
            selectedSquare: null,
            validMoves: [],
        });

        console.log(`Selected drop piece: ${pieceType}, valid drops:`, validDropSquares);
    },

    makeDrop: (pieceType: PieceType, to: Square) => {
        const { board, hands, currentPlayer, moveHistory, historyCursor } = get();

        try {
            const pieceNameMap: Record<PieceType, string> = {
                pawn: "歩",
                lance: "香",
                knight: "桂",
                silver: "銀",
                gold: "金",
                bishop: "角",
                rook: "飛",
                king: "王",
                gyoku: "玉",
            };

            const japaneseName = pieceNameMap[pieceType];
            if (!japaneseName || hands[currentPlayer][japaneseName] <= 0) {
                throw new Error("その駒を持っていません");
            }

            const move: Move = {
                type: "drop",
                to,
                piece: {
                    type: pieceType,
                    owner: currentPlayer,
                    promoted: false,
                },
            };

            const result = applyMove(board, hands, currentPlayer, move);
            const nextPlayer = result.nextTurn;

            // ゲーム状態判定
            let newStatus: GameStatus = "playing";
            if (isInCheck(result.board, nextPlayer)) {
                if (isCheckmate(result.board, result.hands, nextPlayer)) {
                    // 詰みなので、現在のプレイヤー（手を指した方）の勝ち
                    newStatus = currentPlayer === "black" ? "black_win" : "white_win";
                    playGameSound("gameEnd");
                } else {
                    newStatus = "check";
                    playGameSound("check");
                }
            }

            // 履歴の途中から新しい手を指す場合、未来の履歴を削除
            const newMoveHistory =
                historyCursor === HISTORY_CURSOR.LATEST_POSITION
                    ? [...moveHistory, move]
                    : [...moveHistory.slice(0, historyCursor + 1), move];

            set({
                board: result.board,
                hands: result.hands,
                currentPlayer: nextPlayer,
                selectedDropPiece: null,
                validDropSquares: [],
                moveHistory: newMoveHistory,
                historyCursor: HISTORY_CURSOR.LATEST_POSITION, // 新しい手を指したので最新状態にリセット
                gameStatus: newStatus,
            });

            // 駒音を再生（王手・詰みの音を再生してない場合のみ）
            if (newStatus === "playing") {
                playGameSound("piece");
            }

            // タイマーが有効な場合、タイマーを切り替え
            const { timer } = get();
            if (timer.config.mode && timer.activePlayer) {
                get().switchTimer();
            }

            console.log(`Dropped ${pieceType} at ${to.row}${to.column}`);
        } catch (error) {
            console.error("Invalid drop:", error);
            // エラー時は選択状態をクリア
            set({ selectedDropPiece: null, validDropSquares: [] });
        }
    },

    makeMove: (from: Square, to: Square, promote = false) => {
        const { board, hands, currentPlayer, moveHistory, historyCursor } = get();

        try {
            const fromKey = `${from.row}${from.column}` as keyof Board;
            const toKey = `${to.row}${to.column}` as keyof Board;
            const piece = board[fromKey];
            const captured = board[toKey];

            if (!piece) return;

            const move: Move = {
                type: "move",
                from,
                to,
                piece,
                promote,
                captured: captured || null,
            };

            const result = applyMove(board, hands, currentPlayer, move);
            const nextPlayer = result.nextTurn;

            // ゲーム状態判定
            let newStatus: GameStatus = "playing";
            if (isInCheck(result.board, nextPlayer)) {
                if (isCheckmate(result.board, result.hands, nextPlayer)) {
                    // 詰みなので、現在のプレイヤー（手を指した方）の勝ち
                    newStatus = currentPlayer === "black" ? "black_win" : "white_win";
                    playGameSound("gameEnd");
                } else {
                    newStatus = "check";
                    playGameSound("check");
                }
            }

            // 履歴の途中から新しい手を指す場合、未来の履歴を削除
            const newMoveHistory =
                historyCursor === HISTORY_CURSOR.LATEST_POSITION
                    ? [...moveHistory, move]
                    : [...moveHistory.slice(0, historyCursor + 1), move];

            set({
                board: result.board,
                hands: result.hands,
                currentPlayer: nextPlayer,
                selectedSquare: null,
                validMoves: [],
                moveHistory: newMoveHistory,
                historyCursor: HISTORY_CURSOR.LATEST_POSITION, // 新しい手を指したので最新状態にリセット
                gameStatus: newStatus,
            });

            // 駒音を再生（王手・詰みの音を再生してない場合のみ）
            if (newStatus === "playing") {
                playGameSound("piece");
            }

            // タイマーが有効な場合、タイマーを切り替え
            const { timer } = get();
            if (timer.config.mode && timer.activePlayer) {
                get().switchTimer();
            }
        } catch (error) {
            console.error("Invalid move:", error);
        }
    },

    confirmPromotion: (promote: boolean) => {
        const { promotionPending } = get();
        if (!promotionPending) return;

        const { from, to } = promotionPending;
        get().makeMove(from, to, promote);
        set({ promotionPending: null });
    },

    cancelPromotion: () => {
        set({ promotionPending: null });
    },

    clearSelections: () => {
        set({
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
        });
    },

    resetGame: () => {
        set({
            board: modernInitialBoard,
            hands: structuredClone(initialHands()),
            currentPlayer: "black",
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            moveHistory: [],
            historyCursor: HISTORY_CURSOR.LATEST_POSITION,
            gameStatus: "playing",
            promotionPending: null,
            resignedPlayer: null,
            timer: createInitialTimerState(),
        });
    },

    resign: () => {
        const { currentPlayer, gameStatus } = get();

        // ゲーム中でない場合は投了できない
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        // 現在のプレイヤーが投了したので、相手の勝ち
        const winStatus = currentPlayer === "black" ? "white_win" : "black_win";

        set({
            gameStatus: winStatus,
            resignedPlayer: currentPlayer,
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
        });

        // ゲーム終了音を再生
        playGameSound("gameEnd");
    },

    importGame: (moves: Move[], kifContent?: string) => {
        // KIFコンテンツがある場合は初期局面を解析
        let initialBoard = modernInitialBoard;
        let initialHandsData = structuredClone(initialHands());

        if (kifContent) {
            const parseResult = parseKifMoves(kifContent);
            if (parseResult.initialBoard) {
                initialBoard = parseResult.initialBoard;
            }
            if (parseResult.initialHands) {
                initialHandsData = parseResult.initialHands;
            }
        }

        // ゲームをリセットしてから棋譜を読み込む
        set({
            board: initialBoard,
            hands: initialHandsData,
            currentPlayer: "black",
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            moveHistory: moves,
            historyCursor: moves.length - 1, // 最終手の状態に設定
            gameStatus: "playing",
            promotionPending: null,
            resignedPlayer: null,
        });

        // 最終局面まで再構築
        if (moves.length > 0) {
            // 初期局面から手を適用していく
            let board = initialBoard;
            let hands = initialHandsData;
            let currentPlayer: Player = "black";

            for (const move of moves) {
                try {
                    const result = applyMove(board, hands, currentPlayer, move);
                    board = result.board;
                    hands = result.hands;
                    currentPlayer = result.nextTurn;
                } catch (error) {
                    console.error("Failed to apply move:", error);
                    break;
                }
            }

            // ゲーム状態の判定
            let gameStatus: GameStatus = "playing";
            if (isInCheck(board, currentPlayer)) {
                if (isCheckmate(board, hands, currentPlayer)) {
                    gameStatus = "checkmate";
                } else {
                    gameStatus = "check";
                }
            }

            set({
                board,
                hands,
                currentPlayer,
                gameStatus,
            });
        }
    },

    importSfen: (sfen: string) => {
        try {
            // SFEN形式から局面を解析
            const { board, hands, currentPlayer } = parseSfen(sfen);

            // ゲーム状態の判定
            let gameStatus: GameStatus = "playing";

            // 王手判定
            if (isInCheck(board, currentPlayer)) {
                // 詰み判定
                if (isCheckmate(board, hands, currentPlayer)) {
                    gameStatus = "checkmate";
                } else {
                    gameStatus = "check";
                }
            }

            // 局面を設定（履歴はクリア）
            set({
                board,
                hands,
                currentPlayer,
                selectedSquare: null,
                selectedDropPiece: null,
                validMoves: [],
                validDropSquares: [],
                moveHistory: [],
                historyCursor: HISTORY_CURSOR.LATEST_POSITION,
                gameStatus,
                promotionPending: null,
                resignedPlayer: null,
                timer: createInitialTimerState(),
            });
        } catch (error) {
            console.error("Failed to import SFEN:", error);
            throw error;
        }
    },

    // 履歴操作機能
    undo: () => {
        const { moveHistory, historyCursor } = get();
        if (moveHistory.length === 0) return;

        const newCursor =
            historyCursor === HISTORY_CURSOR.LATEST_POSITION
                ? moveHistory.length - 2
                : historyCursor - 1;
        // 初期位置に戻る場合は INITIAL_POSITION を使用
        const finalCursor =
            newCursor <= HISTORY_CURSOR.LATEST_POSITION
                ? HISTORY_CURSOR.INITIAL_POSITION
                : newCursor;
        if (finalCursor < HISTORY_CURSOR.INITIAL_POSITION) return;

        const { board, hands, currentPlayer, gameStatus } = reconstructGameState(
            moveHistory,
            finalCursor,
        );

        set({
            board,
            hands,
            currentPlayer,
            gameStatus,
            historyCursor: finalCursor,
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            promotionPending: null,
        });
    },

    redo: () => {
        const { moveHistory, historyCursor } = get();
        // 最新位置からはredoできない
        if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) return;
        // 最後の手からはredoできない
        if (historyCursor >= moveHistory.length - 1) return;

        // 初期位置からは最初の手（0）に進む
        const newCursor = historyCursor === HISTORY_CURSOR.INITIAL_POSITION ? 0 : historyCursor + 1;
        const { board, hands, currentPlayer, gameStatus } = reconstructGameState(
            moveHistory,
            newCursor,
        );

        set({
            board,
            hands,
            currentPlayer,
            gameStatus,
            historyCursor: newCursor,
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            promotionPending: null,
        });
    },

    goToMove: (moveIndex: number) => {
        const { moveHistory } = get();
        if (moveIndex < HISTORY_CURSOR.INITIAL_POSITION || moveIndex >= moveHistory.length) return;

        const { board, hands, currentPlayer, gameStatus } = reconstructGameState(
            moveHistory,
            moveIndex,
        );

        set({
            board,
            hands,
            currentPlayer,
            gameStatus,
            historyCursor: moveIndex,
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            promotionPending: null,
        });
    },

    canUndo: () => {
        const { moveHistory, historyCursor } = get();
        if (moveHistory.length === 0) return false;

        // 初期位置からはundoできない
        if (historyCursor === HISTORY_CURSOR.INITIAL_POSITION) return false;

        // 最新状態なら最後の手まで戻れる、そうでなければさらに1つ戻れるかチェック
        const newCursor =
            historyCursor === HISTORY_CURSOR.LATEST_POSITION
                ? moveHistory.length - 2
                : historyCursor - 1;
        return newCursor >= HISTORY_CURSOR.INITIAL_POSITION;
    },

    canRedo: () => {
        const { moveHistory, historyCursor } = get();
        // 初期位置からは、手が存在する場合にredoできる
        if (historyCursor === HISTORY_CURSOR.INITIAL_POSITION) return moveHistory.length > 0;
        // 最新位置からはredoできない
        if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) return false;
        // その他の位置からは、最後の手でなければredoできる
        return historyCursor < moveHistory.length - 1;
    },

    // タイマーアクション
    initializeTimer: (config: TimerConfig) => {
        const basicTimeMs = config.basicTime * 1000;
        const isPerMove = config.mode === "perMove";
        const perMoveTimeMs = config.perMoveLimit * 1000;

        set({
            timer: {
                config,
                blackTime: isPerMove ? perMoveTimeMs : basicTimeMs,
                whiteTime: isPerMove ? perMoveTimeMs : basicTimeMs,
                blackInByoyomi: false,
                whiteInByoyomi: false,
                activePlayer: null,
                isPaused: false,
                lastTickTime: Date.now(),
                blackWarningLevel: "normal",
                whiteWarningLevel: "normal",
                hasTimedOut: false,
                timedOutPlayer: null,
            },
        });
    },

    startPlayerTimer: (player: Player) => {
        const { timer } = get();
        if (!timer.config.mode) return;

        set({
            timer: {
                ...timer,
                activePlayer: player,
                isPaused: false,
                lastTickTime: Date.now(),
            },
        });
    },

    switchTimer: () => {
        const { timer, currentPlayer } = get();
        if (!timer.config.mode) return;

        const now = Date.now();
        const { config } = timer;
        const previousPlayer = timer.activePlayer;

        // フィッシャー方式の場合、現在のプレイヤーに時間を加算
        if (config.mode === "fischer" && previousPlayer) {
            const currentTime = previousPlayer === "black" ? timer.blackTime : timer.whiteTime;
            const newTime = currentTime + config.fischerIncrement * 1000;
            set({
                timer: {
                    ...timer,
                    [`${previousPlayer}Time`]: newTime,
                    activePlayer: currentPlayer,
                    lastTickTime: now,
                },
            });
        }
        // 一手制限方式の場合、次のプレイヤーの時間をリセット
        else if (config.mode === "perMove") {
            set({
                timer: {
                    ...timer,
                    [`${currentPlayer}Time`]: config.perMoveLimit * 1000,
                    activePlayer: currentPlayer,
                    lastTickTime: now,
                },
            });
        }
        // 通常の切り替え
        else {
            set({
                timer: {
                    ...timer,
                    activePlayer: currentPlayer,
                    lastTickTime: now,
                },
            });
        }
    },

    pauseTimer: () => {
        const { timer } = get();
        if (!timer.config.mode || timer.isPaused) return;

        set({
            timer: {
                ...timer,
                isPaused: true,
            },
        });
    },

    resumeTimer: () => {
        const { timer } = get();
        if (!timer.config.mode || !timer.isPaused) return;

        set({
            timer: {
                ...timer,
                isPaused: false,
                lastTickTime: Date.now(),
            },
        });
    },

    resetTimer: () => {
        set({ timer: createInitialTimerState() });
    },

    tick: () => {
        const { timer, gameStatus } = get();
        if (!timer.config.mode || timer.isPaused || !timer.activePlayer || timer.hasTimedOut)
            return;
        if (gameStatus !== "playing" && gameStatus !== "check") return;

        const now = Date.now();
        const elapsed = now - timer.lastTickTime;
        const player = timer.activePlayer;
        const currentTime = player === "black" ? timer.blackTime : timer.whiteTime;
        const inByoyomi = player === "black" ? timer.blackInByoyomi : timer.whiteInByoyomi;
        const newTime = Math.max(0, currentTime - elapsed);

        // 基本時間を使い切った場合
        if (newTime === 0 && !inByoyomi && timer.config.mode === "basic") {
            // 秒読みに移行
            set({
                timer: {
                    ...timer,
                    [`${player}Time`]: timer.config.byoyomiTime * 1000,
                    [`${player}InByoyomi`]: true,
                    lastTickTime: now,
                },
            });
            get().updateWarningLevels();
        } else if (newTime === 0 && (inByoyomi || timer.config.mode !== "basic")) {
            // 時間切れ
            const winStatus = player === "black" ? "white_win" : "black_win";
            set({
                timer: {
                    ...timer,
                    hasTimedOut: true,
                    timedOutPlayer: player,
                },
                gameStatus: winStatus,
            });
            playGameSound("gameEnd");
        } else {
            set({
                timer: {
                    ...timer,
                    [`${player}Time`]: newTime,
                    lastTickTime: now,
                },
            });
            get().updateWarningLevels();
        }
    },

    updateWarningLevels: () => {
        const { timer } = get();
        const blackWarningLevel = getWarningLevel(timer.blackTime, timer.blackInByoyomi);
        const whiteWarningLevel = getWarningLevel(timer.whiteTime, timer.whiteInByoyomi);

        if (
            blackWarningLevel !== timer.blackWarningLevel ||
            whiteWarningLevel !== timer.whiteWarningLevel
        ) {
            set({
                timer: {
                    ...timer,
                    blackWarningLevel,
                    whiteWarningLevel,
                },
            });
        }
    },
}));
