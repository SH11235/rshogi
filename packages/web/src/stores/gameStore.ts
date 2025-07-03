import { AIPlayerService } from "@/services/ai/aiPlayer";
import { audioManager } from "@/services/audioManager";
import {
    type ConnectionProgress,
    type ConnectionQuality,
    WebRTCConnection,
} from "@/services/webrtc";
import type { AIDifficulty, AIPlayer } from "@/types/ai";
import type { SoundType } from "@/types/audio";
import type {
    ConnectionStatus,
    DrawOfferMessage,
    GameMessage,
    GameStartMessage,
    JishogiCheckMessage,
    MoveMessage,
    RepetitionCheckMessage,
    ResignMessage,
    TimerConfigMessage,
    TimerUpdateMessage,
    TryRuleMessage,
} from "@/types/online";
import {
    type StateSyncResponseMessage,
    isDrawOfferMessage,
    isGameStartMessage,
    isJishogiCheckMessage,
    isMoveMessage,
    isRepetitionCheckMessage,
    isResignMessage,
    isStateSyncRequestMessage,
    isStateSyncResponseMessage,
    isTimerConfigMessage,
    isTimerUpdateMessage,
    isTryRuleMessage,
} from "@/types/online";
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
    type PositionState,
    type Square,
    type SquareKey,
    applyMove,
    canPromoteByPosition,
    checkJishogi,
    checkPerpetualCheck,
    checkSennichite,
    checkTryRule,
    generateLegalDropMovesForPiece,
    generateLegalMoves,
    initialHands,
    isCheckmate,
    isInCheck,
    modernInitialBoard,
    mustPromote,
    parseKifMoves,
    parseSfen,
    reconstructGameStateWithInitial,
    validateReceivedMove,
} from "shogi-core";
import { MateSearchService } from "shogi-core";
import { create } from "zustand";
import type { MateSearchOptions, MateSearchState } from "../types/mateSearch";

// 駒の日本語名を取得するヘルパー関数
function getPieceJapaneseName(piece: Piece): string {
    const pieceNames: Record<PieceType, string> = {
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

    const promotedNames: Record<PieceType, string> = {
        pawn: "と",
        lance: "成香",
        knight: "成桂",
        silver: "成銀",
        gold: "金",
        bishop: "馬",
        rook: "龍",
        king: "王",
        gyoku: "玉",
    };

    return piece.promoted ? promotedNames[piece.type] : pieceNames[piece.type];
}

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

// 千日手・持将棋の判定用ヘルパー
function checkRepetitionAndJishogi(
    moveHistory: Move[],
    currentBoard: Board,
    currentHands: Hands,
    currentPlayer: Player,
    initialBoard: Board,
    initialHands: Hands,
): {
    isSennichite: boolean;
    isPerpetualCheck: boolean;
    isJishogi: boolean;
    jishogiStatus?: { isJishogi: boolean; blackPoints?: number; whitePoints?: number };
} {
    // 局面履歴を再構築
    const positionHistory: PositionState[] = [];
    const checkHistory: boolean[] = [];

    let board = structuredClone(initialBoard);
    let hands = structuredClone(initialHands);
    let player: Player = "black";

    // 初期局面を追加
    positionHistory.push({
        board: structuredClone(board),
        hands: structuredClone(hands),
        currentPlayer: player,
    });
    checkHistory.push(isInCheck(board, player));

    // 各手を適用して局面履歴を作成
    for (const move of moveHistory) {
        const result = applyMove(board, hands, player, move);
        board = result.board;
        hands = result.hands;
        player = result.nextTurn;

        positionHistory.push({
            board: structuredClone(board),
            hands: structuredClone(hands),
            currentPlayer: player,
        });
        checkHistory.push(isInCheck(board, player));
    }

    // 現在の局面も追加
    positionHistory.push({ board: currentBoard, hands: currentHands, currentPlayer });
    checkHistory.push(isInCheck(currentBoard, currentPlayer));

    // 千日手判定
    const isSennichite = checkSennichite(positionHistory);
    const isPerpetualCheck = checkPerpetualCheck(positionHistory, checkHistory);

    // 持将棋判定
    const jishogiStatus = checkJishogi(currentBoard, currentHands);

    return {
        isSennichite,
        isPerpetualCheck,
        isJishogi: jishogiStatus.isJishogi,
        jishogiStatus,
    };
}

interface PromotionPendingMove {
    from: Square;
    to: Square;
    piece: Piece;
}

// ゲームモードの型定義
type GameMode = "playing" | "review" | "analysis";

// ゲームタイプの型定義
export type GameType = "local" | "ai" | "online";

// 分岐情報の型定義
interface BranchInfo {
    branchPoint: number; // 分岐開始点の手数
    originalMoves: Move[]; // 本譜の手順
    branchMoves: Move[]; // 分岐の手順
    isInBranch: boolean; // 現在分岐中かどうか
}

// 閲覧モードの基準局面
interface ReviewBasePosition {
    board: Board;
    hands: Hands;
    moveIndex: number;
    currentPlayer: Player;
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
    branchInfo: BranchInfo | null; // 分岐情報
    originalMoveHistory: Move[]; // 本譜の保存用
    initialBoard: Board; // 初期局面
    initialHandsData: Hands; // 初期持ち駒
    isTsumeShogi: boolean; // 詰将棋モードかどうか
    gameMode: GameMode; // ゲームモード
    reviewBasePosition: ReviewBasePosition | null; // 閲覧モードの基準局面
    undoStack: Move[]; // 対局中のundo用スタック

    // タイマー状態
    timer: TimerState;

    // 通信対戦関連
    isOnlineGame: boolean;
    connectionStatus: ConnectionStatus;
    connectionQuality: ConnectionQuality | null;
    connectionProgress: ConnectionProgress;
    webrtcConnection: WebRTCConnection | null;
    localPlayer: Player | null; // ローカルプレイヤーの色
    localPlayerName: string; // ローカルプレイヤー名
    remotePlayerName: string; // リモートプレイヤー名

    // 引き分け提案関連
    drawOfferPending: boolean;
    pendingDrawOfferer: Player | null;

    // 詰み探索関連
    mateSearch: MateSearchState;

    // AI対戦関連
    gameType: GameType;
    aiPlayer: AIPlayerService | null;
    aiPlayerInfo: AIPlayer | null;
    isAIThinking: boolean;
    aiDifficulty: AIDifficulty;
    localPlayerColor: Player; // AI対戦時のプレイヤー色

    selectSquare: (square: Square) => void;
    selectDropPiece: (pieceType: PieceType, player: Player) => void;
    makeMove: (from: Square, to: Square, promote?: boolean) => void;
    makeDrop: (pieceType: PieceType, to: Square) => void;
    confirmPromotion: (promote: boolean) => void;
    cancelPromotion: () => void;
    clearSelections: () => void;
    resetGame: () => void;
    resign: () => void;
    offerDraw: () => void;
    acceptDrawOffer: () => void;
    rejectDrawOffer: () => void;
    importGame: (moves: Move[], kifContent?: string) => void;
    importSfen: (sfen: string) => void;
    // 履歴操作機能
    undo: () => void;
    redo: () => void;
    goToMove: (moveIndex: number) => void;
    canUndo: () => boolean;
    canRedo: () => boolean;
    // 再生制御機能
    navigateToMove: (index: number) => void;
    navigateNext: () => void;
    navigatePrevious: () => void;
    navigateFirst: () => void;
    navigateLast: () => void;
    returnToMainLine: () => void;
    isInBranch: () => boolean;
    canNavigateNext: () => boolean;
    canNavigatePrevious: () => boolean;
    // モード管理機能
    setGameMode: (mode: GameMode) => void;
    startGameFromPosition: () => void;
    returnToReviewMode: () => void;
    switchToAnalysisMode: () => void;
    // 対局中のundo/redo機能
    gameUndo: () => void;
    gameRedo: () => void;
    canGameUndo: () => boolean;
    canGameRedo: () => boolean;

    // 通信対戦機能
    startOnlineGame: (isHost: boolean, playerName?: string) => Promise<string>;
    joinOnlineGame: (offer: string, playerName?: string) => Promise<string>;
    acceptOnlineAnswer: (answer: string) => Promise<void>;
    handleOnlineMessage: (message: GameMessage) => void;
    disconnectOnline: () => void;

    // 詰み探索機能
    startMateSearch: (options?: Partial<MateSearchOptions>) => Promise<void>;
    cancelMateSearch: () => void;

    // AI対戦機能
    startAIGame: (difficulty?: AIDifficulty, playerColor?: Player) => Promise<void>;
    setAIDifficulty: (difficulty: AIDifficulty) => Promise<void>;
    stopAI: () => void;
    executeAIMove: () => Promise<void>;
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
            considerationTime: 0,
            considerationCount: 0,
        },
        blackTime: 0,
        whiteTime: 0,
        blackInByoyomi: false,
        whiteInByoyomi: false,
        blackConsiderationsRemaining: 0,
        whiteConsiderationsRemaining: 0,
        isUsingConsideration: false,
        considerationStartTime: null,
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
    branchInfo: null,
    originalMoveHistory: [],
    initialBoard: modernInitialBoard,
    initialHandsData: structuredClone(initialHands()),
    isTsumeShogi: false,
    gameMode: "playing",
    reviewBasePosition: null,
    undoStack: [],
    timer: createInitialTimerState(),

    // 通信対戦関連の初期状態
    isOnlineGame: false,
    connectionStatus: {
        isConnected: false,
        isHost: false,
        peerId: "",
        connectionState: "new",
    },
    connectionQuality: null,
    connectionProgress: "idle" as ConnectionProgress,
    webrtcConnection: null,
    localPlayer: null,
    localPlayerName: "",
    remotePlayerName: "",

    // 引き分け提案関連
    drawOfferPending: false,
    pendingDrawOfferer: null,

    // 詰み探索の初期状態
    mateSearch: {
        status: "idle",
        depth: 0,
        maxDepth: 7,
        result: null,
        error: null,
    },

    // AI対戦関連の初期状態
    gameType: "local",
    aiPlayer: null,
    aiPlayerInfo: null,
    isAIThinking: false,
    aiDifficulty: "intermediate",
    localPlayerColor: "black",

    selectSquare: (square: Square) => {
        const {
            board,
            currentPlayer,
            selectedSquare,
            selectedDropPiece,
            validDropSquares,
            gameStatus,
            gameMode,
        } = get();

        // 閲覧モードでは操作不可
        if (gameMode === "review") {
            return;
        }

        // ゲーム終了時は操作不可（対局・解析モードのみ）
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        // 通信対戦時は自分の手番でのみ操作可能
        const { isOnlineGame, localPlayer, gameType, localPlayerColor, isAIThinking } = get();
        if (isOnlineGame && localPlayer !== currentPlayer) {
            return;
        }

        // AI対戦時はAIの手番またはAI思考中は操作不可
        if (gameType === "ai" && (currentPlayer !== localPlayerColor || isAIThinking)) {
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
        const { board, hands, currentPlayer, gameStatus, gameMode } = get();

        // 閲覧モードでは操作不可
        if (gameMode === "review") {
            return;
        }

        // ゲーム終了時は操作不可
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        // 手番プレイヤーでない場合は無視
        if (player !== currentPlayer) {
            return;
        }

        // 通信対戦時は自分の手番でのみ操作可能
        const { isOnlineGame, localPlayer } = get();
        if (isOnlineGame && localPlayer !== currentPlayer) {
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
        const { board, hands, currentPlayer, moveHistory, historyCursor, gameMode } = get();

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

            // 履歴の途中から新しい手を指す場合、分岐を作成
            const { branchInfo, originalMoveHistory } = get();
            let newMoveHistory: Move[];
            let newBranchInfo = branchInfo;
            let newOriginalMoveHistory = originalMoveHistory;
            let newGameMode = gameMode;

            if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) {
                // 最新位置から指す場合は通常通り追加
                newMoveHistory = [...moveHistory, move];
            } else {
                // 履歴の途中から指す場合は分岐を作成
                if (!branchInfo || !branchInfo.isInBranch) {
                    // 新しい分岐を作成
                    newOriginalMoveHistory = [...moveHistory]; // 現在の履歴を本譜として保存
                    newBranchInfo = {
                        branchPoint: historyCursor,
                        originalMoves: moveHistory,
                        branchMoves: [move],
                        isInBranch: true,
                    };
                    newMoveHistory = [...moveHistory.slice(0, historyCursor + 1), move];

                    // 閲覧モードから分岐を作成した場合は解析モードに移行
                    if (gameMode === "review") {
                        newGameMode = "analysis";
                    }
                } else {
                    // 既に分岐中の場合は分岐を延長
                    newBranchInfo = {
                        ...branchInfo,
                        branchMoves: [...branchInfo.branchMoves, move],
                    };
                    newMoveHistory = [...moveHistory.slice(0, historyCursor + 1), move];
                }
            }

            // トライルール判定（対局中のみ）
            if (gameMode === "playing" && newStatus === "playing") {
                if (checkTryRule(result.board, currentPlayer)) {
                    newStatus = currentPlayer === "black" ? "try_rule_black" : "try_rule_white";
                    console.log("Try rule victory:", currentPlayer);
                    playGameSound("gameEnd");
                }
            }

            // 千日手・持将棋の判定（対局中のみ）
            if (gameMode === "playing" && newStatus === "playing") {
                const { initialBoard, initialHandsData } = get();
                const repetitionCheck = checkRepetitionAndJishogi(
                    newMoveHistory,
                    result.board,
                    result.hands,
                    nextPlayer,
                    initialBoard,
                    initialHandsData,
                );

                if (repetitionCheck.isSennichite) {
                    if (repetitionCheck.isPerpetualCheck) {
                        // 連続王手の千日手は王手をかけている側の負け
                        newStatus = currentPlayer === "black" ? "black_win" : "white_win";
                        console.log("Perpetual check - winner:", newStatus);
                    } else {
                        // 通常の千日手は引き分け
                        newStatus = "sennichite";
                        console.log("Sennichite (repetition) detected");
                    }
                    playGameSound("gameEnd");
                } else if (repetitionCheck.isJishogi) {
                    // 持将棋
                    newStatus = "draw";
                    console.log("Jishogi (impasse) detected:", repetitionCheck.jishogiStatus);
                    playGameSound("gameEnd");
                }
            }

            set({
                board: result.board,
                hands: result.hands,
                currentPlayer: nextPlayer,
                selectedDropPiece: null,
                validDropSquares: [],
                moveHistory: newMoveHistory,
                historyCursor: HISTORY_CURSOR.LATEST_POSITION, // 新しい手を指したので最新状態にリセット
                gameStatus: newStatus,
                branchInfo: newBranchInfo,
                originalMoveHistory: newOriginalMoveHistory,
                gameMode: newGameMode,
                undoStack: [], // 新しい手を指したのでundoStackをクリア
                isAIThinking: false, // AI思考フラグをリセット
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

            // 通信対戦時は相手にメッセージを送信
            const { isOnlineGame, webrtcConnection, localPlayer } = get();
            if (isOnlineGame && webrtcConnection && localPlayer === currentPlayer) {
                const moveMessage: MoveMessage = {
                    type: "move",
                    data: {
                        from: "" as SquareKey, // dropの場合は空文字列
                        to: `${to.row}${to.column}` as SquareKey,
                        drop: pieceType,
                    },
                    timestamp: Date.now(),
                    playerId: webrtcConnection.getConnectionInfo().peerId,
                };
                webrtcConnection.sendMessage(moveMessage);
            }

            console.log(`Dropped ${pieceType} at ${to.row}${to.column}`);

            // AI対戦時、人間の手の後にAIに思考させる
            const { gameType, localPlayerColor } = get();
            if (
                gameType === "ai" &&
                (newStatus === "playing" || newStatus === "check") &&
                nextPlayer !== localPlayerColor
            ) {
                // 少し遅延を入れてからAIを実行
                setTimeout(() => {
                    get().executeAIMove();
                }, 500);
            }
        } catch (error) {
            console.error("Invalid drop:", error);
            // エラー時は選択状態をクリア
            set({ selectedDropPiece: null, validDropSquares: [] });
        }
    },

    makeMove: (from: Square, to: Square, promote = false) => {
        const {
            board,
            hands,
            currentPlayer,
            moveHistory,
            historyCursor,
            gameMode,
            isOnlineGame,
            webrtcConnection,
            localPlayer,
        } = get();

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

            // 履歴の途中から新しい手を指す場合、分岐を作成
            const { branchInfo, originalMoveHistory } = get();
            let newMoveHistory: Move[];
            let newBranchInfo = branchInfo;
            let newOriginalMoveHistory = originalMoveHistory;
            let newGameMode = gameMode;

            if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) {
                // 最新位置から指す場合は通常通り追加
                newMoveHistory = [...moveHistory, move];
            } else {
                // 履歴の途中から指す場合は分岐を作成
                if (!branchInfo || !branchInfo.isInBranch) {
                    // 新しい分岐を作成
                    newOriginalMoveHistory = [...moveHistory]; // 現在の履歴を本譜として保存
                    newBranchInfo = {
                        branchPoint: historyCursor,
                        originalMoves: moveHistory,
                        branchMoves: [move],
                        isInBranch: true,
                    };
                    newMoveHistory = [...moveHistory.slice(0, historyCursor + 1), move];

                    // 閲覧モードから分岐を作成した場合は解析モードに移行
                    if (gameMode === "review") {
                        newGameMode = "analysis";
                    }
                } else {
                    // 既に分岐中の場合は分岐を延長
                    newBranchInfo = {
                        ...branchInfo,
                        branchMoves: [...branchInfo.branchMoves, move],
                    };
                    newMoveHistory = [...moveHistory.slice(0, historyCursor + 1), move];
                }
            }

            // トライルール判定（対局中のみ）
            if (gameMode === "playing" && newStatus === "playing") {
                if (checkTryRule(result.board, currentPlayer)) {
                    newStatus = currentPlayer === "black" ? "try_rule_black" : "try_rule_white";
                    console.log("Try rule victory:", currentPlayer);
                    playGameSound("gameEnd");
                }
            }

            // 千日手・持将棋の判定（対局中のみ）
            if (gameMode === "playing" && newStatus === "playing") {
                const { initialBoard, initialHandsData } = get();
                const repetitionCheck = checkRepetitionAndJishogi(
                    newMoveHistory,
                    result.board,
                    result.hands,
                    nextPlayer,
                    initialBoard,
                    initialHandsData,
                );

                if (repetitionCheck.isSennichite) {
                    if (repetitionCheck.isPerpetualCheck) {
                        // 連続王手の千日手は王手をかけている側の負け
                        newStatus = currentPlayer === "black" ? "black_win" : "white_win";
                        console.log("Perpetual check - winner:", newStatus);
                    } else {
                        // 通常の千日手は引き分け
                        newStatus = "sennichite";
                        console.log("Sennichite (repetition) detected");
                    }
                    playGameSound("gameEnd");
                } else if (repetitionCheck.isJishogi) {
                    // 持将棋
                    newStatus = "draw";
                    console.log("Jishogi (impasse) detected:", repetitionCheck.jishogiStatus);
                    playGameSound("gameEnd");
                }
            }

            set({
                board: result.board,
                hands: result.hands,
                currentPlayer: nextPlayer,
                selectedSquare: null,
                validMoves: [],
                moveHistory: newMoveHistory,
                historyCursor: HISTORY_CURSOR.LATEST_POSITION, // 新しい手を指したので最新状態にリセット
                gameStatus: newStatus,
                branchInfo: newBranchInfo,
                originalMoveHistory: newOriginalMoveHistory,
                gameMode: newGameMode,
                undoStack: [], // 新しい手を指したのでundoStackをクリア
                isAIThinking: false, // AI思考フラグをリセット
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

            // 通信対戦時は相手にメッセージを送信
            if (isOnlineGame && webrtcConnection && localPlayer === currentPlayer) {
                const moveMessage: MoveMessage = {
                    type: "move",
                    data: {
                        from: `${from.row}${from.column}` as SquareKey,
                        to: `${to.row}${to.column}` as SquareKey,
                        promote,
                    },
                    timestamp: Date.now(),
                    playerId: webrtcConnection.getConnectionInfo().peerId,
                };
                webrtcConnection.sendMessage(moveMessage);

                // トライルール判定結果を送信
                if (
                    gameMode === "playing" &&
                    (newStatus === "try_rule_black" || newStatus === "try_rule_white")
                ) {
                    const tryRuleMessage: TryRuleMessage = {
                        type: "try_rule",
                        data: {
                            winner: currentPlayer,
                        },
                        timestamp: Date.now(),
                        playerId: webrtcConnection.getConnectionInfo().peerId,
                    };
                    webrtcConnection.sendMessage(tryRuleMessage);
                }

                // 千日手・持将棋の判定結果も送信
                if (
                    gameMode === "playing" &&
                    (newStatus === "sennichite" ||
                        newStatus === "draw" ||
                        (newStatus !== "playing" && newStatus !== "check"))
                ) {
                    const { initialBoard, initialHandsData } = get();
                    const repetitionCheck = checkRepetitionAndJishogi(
                        newMoveHistory,
                        result.board,
                        result.hands,
                        nextPlayer,
                        initialBoard,
                        initialHandsData,
                    );

                    if (repetitionCheck.isSennichite) {
                        const repetitionMessage: RepetitionCheckMessage = {
                            type: "repetition_check",
                            data: {
                                isSennichite: true,
                                isPerpetualCheck: repetitionCheck.isPerpetualCheck,
                            },
                            timestamp: Date.now(),
                            playerId: webrtcConnection.getConnectionInfo().peerId,
                        };
                        webrtcConnection.sendMessage(repetitionMessage);
                    }

                    if (repetitionCheck.isJishogi && repetitionCheck.jishogiStatus) {
                        const jishogiMessage: JishogiCheckMessage = {
                            type: "jishogi_check",
                            data: {
                                isJishogi: true,
                                blackPoints: repetitionCheck.jishogiStatus.blackPoints || 0,
                                whitePoints: repetitionCheck.jishogiStatus.whitePoints || 0,
                            },
                            timestamp: Date.now(),
                            playerId: webrtcConnection.getConnectionInfo().peerId,
                        };
                        webrtcConnection.sendMessage(jishogiMessage);
                    }
                }
            }

            // AI対戦時、人間の手の後にAIに思考させる
            const { gameType, localPlayerColor } = get();
            if (
                gameType === "ai" &&
                (newStatus === "playing" || newStatus === "check") &&
                nextPlayer !== localPlayerColor
            ) {
                // 少し遅延を入れてからAIを実行
                setTimeout(() => {
                    get().executeAIMove();
                }, 500);
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
        // AIを停止
        const { aiPlayer } = get();
        if (aiPlayer) {
            aiPlayer.dispose();
        }

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
            branchInfo: null,
            originalMoveHistory: [],
            initialBoard: modernInitialBoard,
            initialHandsData: structuredClone(initialHands()),
            isTsumeShogi: false,
            gameMode: "playing",
            reviewBasePosition: null,
            timer: createInitialTimerState(),
            // AI関連をリセット
            gameType: "local",
            aiPlayer: null,
            aiPlayerInfo: null,
            isAIThinking: false,
        });
    },

    resign: () => {
        const { currentPlayer, gameStatus, isOnlineGame, webrtcConnection, localPlayer } = get();

        // ゲーム中でない場合は投了できない
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        // 通信対戦時は自分の手番でのみ投了可能
        if (isOnlineGame && localPlayer !== currentPlayer) {
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

        // 通信対戦時は相手に投了メッセージを送信
        if (isOnlineGame && webrtcConnection) {
            const resignMessage: ResignMessage = {
                type: "resign",
                data: null,
                timestamp: Date.now(),
                playerId: webrtcConnection.getConnectionInfo().peerId,
            };
            webrtcConnection.sendMessage(resignMessage);
        }
    },

    offerDraw: () => {
        const { currentPlayer, gameStatus, isOnlineGame, webrtcConnection, localPlayer } = get();

        // ゲーム中でない場合は引き分け提案できない
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        // 通信対戦時は自分の手番でのみ提案可能
        if (isOnlineGame && localPlayer !== currentPlayer) {
            return;
        }

        // 通信対戦時
        if (isOnlineGame && webrtcConnection) {
            const drawOfferMessage: DrawOfferMessage = {
                type: "draw_offer",
                data: { accepted: undefined }, // 提案メッセージ
                timestamp: Date.now(),
                playerId: webrtcConnection.getConnectionInfo().peerId,
            };
            webrtcConnection.sendMessage(drawOfferMessage);
            set({ drawOfferPending: true, pendingDrawOfferer: localPlayer });
        } else {
            // ローカル対戦時は相手に提案を表示
            set({ drawOfferPending: true, pendingDrawOfferer: currentPlayer });
        }
    },

    acceptDrawOffer: () => {
        const { isOnlineGame, webrtcConnection } = get();

        // 引き分けにする
        set({
            gameStatus: "draw",
            drawOfferPending: false,
            pendingDrawOfferer: null,
        });
        playGameSound("gameEnd");

        // 通信対戦時は承認メッセージを送信
        if (isOnlineGame && webrtcConnection) {
            const acceptMessage: DrawOfferMessage = {
                type: "draw_offer",
                data: { accepted: true },
                timestamp: Date.now(),
                playerId: webrtcConnection.getConnectionInfo().peerId,
            };
            webrtcConnection.sendMessage(acceptMessage);
        }
    },

    rejectDrawOffer: () => {
        const { isOnlineGame, webrtcConnection } = get();

        // 提案を拒否
        set({
            drawOfferPending: false,
            pendingDrawOfferer: null,
        });

        // 通信対戦時は拒否メッセージを送信
        if (isOnlineGame && webrtcConnection) {
            const rejectMessage: DrawOfferMessage = {
                type: "draw_offer",
                data: { accepted: false },
                timestamp: Date.now(),
                playerId: webrtcConnection.getConnectionInfo().peerId,
            };
            webrtcConnection.sendMessage(rejectMessage);
        }
    },

    importGame: (moves: Move[], kifContent?: string) => {
        // KIFコンテンツがある場合は初期局面を解析
        let initialBoard = modernInitialBoard;
        let initialHandsData = structuredClone(initialHands());
        let isTsumeShogi = false;

        if (kifContent) {
            const parseResult = parseKifMoves(kifContent);
            if (parseResult.initialBoard) {
                initialBoard = parseResult.initialBoard;
            }
            if (parseResult.initialHands) {
                initialHandsData = parseResult.initialHands;
            }

            // 詰将棋かどうかを判定
            isTsumeShogi = kifContent.includes("棋戦：詰将棋") || kifContent.includes("詰将棋");
        }

        // 初期局面のゲーム状態を判定
        let initialGameStatus: GameStatus = "playing";
        if (isInCheck(initialBoard, "black")) {
            if (isCheckmate(initialBoard, initialHandsData, "black")) {
                initialGameStatus = "checkmate";
            } else {
                initialGameStatus = "check";
            }
        }

        // ゲームをリセットしてから棋譜を読み込む（初期局面から開始）
        set({
            board: initialBoard,
            hands: initialHandsData,
            currentPlayer: "black",
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            moveHistory: moves,
            historyCursor: HISTORY_CURSOR.INITIAL_POSITION, // 初期局面から開始
            gameStatus: initialGameStatus,
            promotionPending: null,
            resignedPlayer: null,
            initialBoard: initialBoard, // 初期局面を保存
            initialHandsData: initialHandsData, // 初期持ち駒を保存
            branchInfo: null,
            originalMoveHistory: [],
            isTsumeShogi: isTsumeShogi,
            gameMode: "review", // 棋譜インポート時は閲覧モード
            reviewBasePosition: null,
            gameType: "local", // 棋譜インポート時はローカル対局に戻す
            aiPlayer: null,
            aiPlayerInfo: null,
            isAIThinking: false,
        });
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
                initialBoard: board,
                initialHandsData: hands,
                isTsumeShogi: false,
                gameMode: "review", // SFENインポート時も閲覧モード
                reviewBasePosition: null,
                timer: createInitialTimerState(),
            });
        } catch (error) {
            console.error("Failed to import SFEN:", error);
            throw error;
        }
    },

    // 履歴操作機能
    undo: () => {
        const { moveHistory, historyCursor, initialBoard, initialHandsData } = get();
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

        const { board, hands, currentPlayer, gameStatus } = reconstructGameStateWithInitial(
            moveHistory,
            finalCursor,
            initialBoard,
            initialHandsData,
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
        const { moveHistory, historyCursor, initialBoard, initialHandsData } = get();
        // 最新位置からはredoできない
        if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) return;
        // 最後の手からはredoできない
        if (historyCursor >= moveHistory.length - 1) return;

        // 初期位置からは最初の手（0）に進む
        const newCursor = historyCursor === HISTORY_CURSOR.INITIAL_POSITION ? 0 : historyCursor + 1;
        const { board, hands, currentPlayer, gameStatus } = reconstructGameStateWithInitial(
            moveHistory,
            newCursor,
            initialBoard,
            initialHandsData,
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
        const { moveHistory, initialBoard, initialHandsData } = get();
        if (moveIndex < HISTORY_CURSOR.INITIAL_POSITION || moveIndex >= moveHistory.length) return;

        const { board, hands, currentPlayer, gameStatus } = reconstructGameStateWithInitial(
            moveHistory,
            moveIndex,
            initialBoard,
            initialHandsData,
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
        const isConsideration = config.mode === "consideration";

        set({
            timer: {
                config,
                blackTime: isPerMove ? perMoveTimeMs : basicTimeMs,
                whiteTime: isPerMove ? perMoveTimeMs : basicTimeMs,
                blackInByoyomi: false,
                whiteInByoyomi: false,
                blackConsiderationsRemaining: isConsideration ? config.considerationCount : 0,
                whiteConsiderationsRemaining: isConsideration ? config.considerationCount : 0,
                isUsingConsideration: false,
                considerationStartTime: null,
                activePlayer: null,
                isPaused: false,
                lastTickTime: Date.now(),
                blackWarningLevel: "normal",
                whiteWarningLevel: "normal",
                hasTimedOut: false,
                timedOutPlayer: null,
            },
        });

        // オンラインゲームの場合、タイマー設定を相手に送信
        const { isOnlineGame, webrtcConnection, connectionStatus } = get();
        if (isOnlineGame && webrtcConnection && connectionStatus.isHost) {
            const timerConfigMessage: TimerConfigMessage = {
                type: "timer_config",
                data: {
                    mode: config.mode,
                    basicTime: config.basicTime,
                    byoyomiTime: config.byoyomiTime,
                    fischerIncrement: config.fischerIncrement,
                    perMoveLimit: config.perMoveLimit,
                    considerationTime: config.considerationTime,
                    considerationCount: config.considerationCount,
                },
                timestamp: Date.now(),
                playerId: webrtcConnection.getConnectionInfo().peerId,
            };
            webrtcConnection.sendMessage(timerConfigMessage);
        }
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
        const { timer, gameStatus, isOnlineGame, webrtcConnection, localPlayer } = get();
        if (!timer.config.mode || timer.isPaused || !timer.activePlayer || timer.hasTimedOut)
            return;
        if (gameStatus !== "playing" && gameStatus !== "check") return;

        const now = Date.now();
        const player = timer.activePlayer;

        // 考慮時間使用中の場合
        if (timer.isUsingConsideration && timer.considerationStartTime) {
            const considerationElapsed = now - timer.considerationStartTime;
            const considerationLimitMs = timer.config.considerationTime * 1000;

            if (considerationElapsed >= considerationLimitMs) {
                // 考慮時間を使い切った
                set({
                    timer: {
                        ...timer,
                        isUsingConsideration: false,
                        considerationStartTime: null,
                        lastTickTime: now,
                    },
                });
            }
            // 考慮時間中は通常の時間を減らさない
            return;
        }

        const elapsed = now - timer.lastTickTime;
        const currentTime = player === "black" ? timer.blackTime : timer.whiteTime;
        const inByoyomi = player === "black" ? timer.blackInByoyomi : timer.whiteInByoyomi;
        const newTime = Math.max(0, currentTime - elapsed);

        // 基本時間を使い切った場合
        if (
            newTime === 0 &&
            !inByoyomi &&
            (timer.config.mode === "basic" || timer.config.mode === "consideration")
        ) {
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
        } else if (
            newTime === 0 &&
            (inByoyomi || (timer.config.mode !== "basic" && timer.config.mode !== "consideration"))
        ) {
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

        // オンラインゲームで自分のタイマーが更新された場合、相手に送信
        if (isOnlineGame && webrtcConnection && player === localPlayer) {
            const updatedTimer = get().timer;
            const timerUpdateMessage: TimerUpdateMessage = {
                type: "timer_update",
                data: {
                    blackTime: updatedTimer.blackTime,
                    whiteTime: updatedTimer.whiteTime,
                    blackInByoyomi: updatedTimer.blackInByoyomi,
                    whiteInByoyomi: updatedTimer.whiteInByoyomi,
                    activePlayer: updatedTimer.activePlayer,
                },
                timestamp: Date.now(),
                playerId: webrtcConnection.getConnectionInfo().peerId,
            };
            webrtcConnection.sendMessage(timerUpdateMessage);
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

    useConsideration: () => {
        const { timer, gameStatus } = get();
        if (!timer.config.mode || timer.config.mode !== "consideration") return;
        if (gameStatus !== "playing" && gameStatus !== "check") return;
        if (timer.isUsingConsideration || !timer.activePlayer) return;

        const player = timer.activePlayer;
        const considerationsRemaining =
            player === "black"
                ? timer.blackConsiderationsRemaining
                : timer.whiteConsiderationsRemaining;

        if (considerationsRemaining <= 0) return;

        // 考慮時間を開始
        set({
            timer: {
                ...timer,
                isUsingConsideration: true,
                considerationStartTime: Date.now(),
                [`${player}ConsiderationsRemaining`]: considerationsRemaining - 1,
            },
        });
    },

    cancelConsideration: () => {
        const { timer } = get();
        if (!timer.isUsingConsideration || !timer.considerationStartTime) return;

        const now = Date.now();
        const usedTime = now - timer.considerationStartTime;
        const remainingTime = Math.max(0, timer.config.considerationTime * 1000 - usedTime);

        // 使わなかった時間を持ち時間に追加
        const player = timer.activePlayer;
        if (player) {
            const currentTime = player === "black" ? timer.blackTime : timer.whiteTime;
            set({
                timer: {
                    ...timer,
                    isUsingConsideration: false,
                    considerationStartTime: null,
                    [`${player}Time`]: currentTime + remainingTime,
                },
            });
        }
    },

    // 再生制御機能
    navigateToMove: (index: number) => {
        const { moveHistory } = get();
        if (index < HISTORY_CURSOR.INITIAL_POSITION || index >= moveHistory.length) return;

        get().goToMove(index);
    },

    navigateNext: () => {
        const { historyCursor, moveHistory } = get();
        if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) return;

        if (historyCursor === HISTORY_CURSOR.INITIAL_POSITION) {
            get().goToMove(0);
        } else if (historyCursor < moveHistory.length - 1) {
            get().goToMove(historyCursor + 1);
        }
    },

    navigatePrevious: () => {
        const { historyCursor, moveHistory } = get();

        if (historyCursor === HISTORY_CURSOR.INITIAL_POSITION) return;

        if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) {
            get().goToMove(moveHistory.length - 2);
        } else if (historyCursor === 0) {
            get().goToMove(HISTORY_CURSOR.INITIAL_POSITION);
        } else {
            get().goToMove(historyCursor - 1);
        }
    },

    navigateFirst: () => {
        get().goToMove(HISTORY_CURSOR.INITIAL_POSITION);
    },

    navigateLast: () => {
        const { moveHistory } = get();
        if (moveHistory.length > 0) {
            get().goToMove(moveHistory.length - 1);
        }
    },

    returnToMainLine: () => {
        const { branchInfo, gameMode } = get();
        if (!branchInfo || !branchInfo.isInBranch) return;

        // 本譜に戻す
        set({
            moveHistory: branchInfo.originalMoves,
            branchInfo: null,
            originalMoveHistory: [],
            historyCursor: branchInfo.branchPoint,
            gameMode: gameMode === "analysis" ? "review" : gameMode, // 解析モードから戻る場合は閲覧モードに
        });

        // 分岐点の局面を再構築
        get().goToMove(branchInfo.branchPoint);
    },

    isInBranch: () => {
        const { branchInfo } = get();
        return branchInfo?.isInBranch ?? false;
    },

    canNavigateNext: () => {
        const { historyCursor, moveHistory } = get();
        if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) return false;
        if (historyCursor === HISTORY_CURSOR.INITIAL_POSITION) return moveHistory.length > 0;
        return historyCursor < moveHistory.length - 1;
    },

    canNavigatePrevious: () => {
        const { historyCursor, moveHistory } = get();
        if (historyCursor === HISTORY_CURSOR.INITIAL_POSITION) return false;
        if (historyCursor === HISTORY_CURSOR.LATEST_POSITION) return moveHistory.length > 0;
        return true;
    },

    // モード管理機能
    setGameMode: (mode: GameMode) => {
        set({ gameMode: mode });
    },

    startGameFromPosition: () => {
        const { board, hands, currentPlayer, historyCursor, moveHistory, gameMode } = get();

        // 現在の局面から対局を開始
        const basePosition: ReviewBasePosition = {
            board: structuredClone(board),
            hands: structuredClone(hands),
            moveIndex: historyCursor,
            currentPlayer,
        };

        // 履歴を現在の位置までに切り詰め
        const newMoveHistory =
            historyCursor === HISTORY_CURSOR.INITIAL_POSITION
                ? []
                : moveHistory.slice(0, historyCursor + 1);

        set({
            gameMode: "playing",
            reviewBasePosition: gameMode === "review" ? basePosition : null,
            moveHistory: newMoveHistory,
            historyCursor: HISTORY_CURSOR.LATEST_POSITION,
            branchInfo: null,
            originalMoveHistory: [],
            gameStatus: "playing",
            resignedPlayer: null,
            gameType: "local", // 局面から対局開始時はローカル対局に設定
        });
    },

    returnToReviewMode: () => {
        const { reviewBasePosition } = get();

        if (!reviewBasePosition) return;

        // 閲覧モードに戻る
        set({
            gameMode: "review",
            board: reviewBasePosition.board,
            hands: reviewBasePosition.hands,
            currentPlayer: reviewBasePosition.currentPlayer,
            historyCursor: reviewBasePosition.moveIndex,
            gameStatus: "playing",
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            promotionPending: null,
        });
    },

    switchToAnalysisMode: () => {
        const { gameMode } = get();
        // 閲覧モードから解析モードに切り替え
        if (gameMode === "review") {
            set({ gameMode: "analysis" });
        }
    },

    // 対局中のundo/redo機能
    gameUndo: () => {
        const { gameMode, moveHistory, undoStack } = get();

        // 対局モード以外では使用不可
        if (gameMode !== "playing") return;

        // undo可能な手がない
        if (moveHistory.length === 0) return;

        // 最後の手を取り出してundoStackに追加
        const lastMove = moveHistory[moveHistory.length - 1];
        const newMoveHistory = moveHistory.slice(0, -1);
        const newUndoStack = [...undoStack, lastMove];

        // 手を戻す前の状態を再構築
        const { initialBoard, initialHandsData } = get();
        const { board, hands, currentPlayer, gameStatus } = reconstructGameStateWithInitial(
            newMoveHistory,
            newMoveHistory.length - 1,
            initialBoard,
            initialHandsData,
        );

        set({
            board,
            hands,
            currentPlayer,
            gameStatus,
            moveHistory: newMoveHistory,
            undoStack: newUndoStack,
            historyCursor: HISTORY_CURSOR.LATEST_POSITION,
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            promotionPending: null,
        });
    },

    gameRedo: () => {
        const { gameMode, undoStack } = get();

        // 対局モード以外では使用不可
        if (gameMode !== "playing") return;

        // redo可能な手がない
        if (undoStack.length === 0) return;

        // undoStackから最後の手を取り出す
        const nextMove = undoStack[undoStack.length - 1];
        const newUndoStack = undoStack.slice(0, -1);

        // 手を進める
        const { board, hands, currentPlayer, moveHistory } = get();
        const result = applyMove(board, hands, currentPlayer, nextMove);

        // ゲーム状態判定
        let gameStatus: GameStatus = "playing";
        if (isInCheck(result.board, result.nextTurn)) {
            if (isCheckmate(result.board, result.hands, result.nextTurn)) {
                gameStatus = "checkmate";
            } else {
                gameStatus = "check";
            }
        }

        set({
            board: result.board,
            hands: result.hands,
            currentPlayer: result.nextTurn,
            gameStatus,
            moveHistory: [...moveHistory, nextMove],
            undoStack: newUndoStack,
            historyCursor: HISTORY_CURSOR.LATEST_POSITION,
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            promotionPending: null,
        });

        // 駒音再生
        playGameSound("piece");

        // 王手音再生
        if (gameStatus === "check") {
            playGameSound("check");
        }

        // ゲーム終了音再生
        if (gameStatus === "checkmate") {
            playGameSound("gameEnd");
        }
    },

    canGameUndo: () => {
        const { gameMode, moveHistory, isOnlineGame } = get();
        // 通信対局中は無効化
        if (isOnlineGame) return false;
        return gameMode === "playing" && moveHistory.length > 0;
    },

    canGameRedo: () => {
        const { gameMode, undoStack, isOnlineGame } = get();
        // 通信対局中は無効化
        if (isOnlineGame) return false;
        return gameMode === "playing" && undoStack.length > 0;
    },

    // 通信対戦機能の実装
    startOnlineGame: async (isHost: boolean, playerName?: string) => {
        const connection = new WebRTCConnection();

        // メッセージハンドラーを設定
        connection.onMessage((message) => {
            get().handleOnlineMessage(message);
        });

        // 接続状態変更ハンドラーを設定
        connection.onConnectionStateChange((state) => {
            set({
                connectionStatus: {
                    ...get().connectionStatus,
                    connectionState: state as ConnectionStatus["connectionState"],
                    isConnected: state === "connected",
                },
            });
        });

        // エラーハンドラーを設定
        connection.onError((error) => {
            console.error("WebRTC error in game:", error);
            // ユーザーに通知するためのエラー状態を設定
            set({
                connectionStatus: {
                    ...get().connectionStatus,
                    connectionState: "failed",
                    isConnected: false,
                },
            });
            // ユーザーフレンドリーなエラーメッセージを表示
            if (error.getUserMessage) {
                console.error("User-friendly error:", error.getUserMessage());
            }
        });

        // 接続品質ハンドラーを設定
        connection.onQualityChange((quality) => {
            set({ connectionQuality: quality });
        });

        // 接続進捗ハンドラーを設定
        connection.onProgressChange((progress) => {
            set({ connectionProgress: progress });
        });

        let offer = "";
        if (isHost) {
            offer = await connection.createHost();
            set({
                isOnlineGame: true,
                webrtcConnection: connection,
                localPlayer: "black", // ホストは先手
                localPlayerName: playerName || "",
                connectionStatus: {
                    isConnected: false,
                    isHost: true,
                    peerId: connection.getConnectionInfo().peerId,
                    connectionState: "connecting",
                },
            });
        }

        get().resetGame();
        set({ gameMode: "playing" });

        return offer;
    },

    joinOnlineGame: async (offer: string, playerName?: string) => {
        const connection = new WebRTCConnection();

        // メッセージハンドラーを設定
        connection.onMessage((message) => {
            get().handleOnlineMessage(message);
        });

        // 接続状態変更ハンドラーを設定
        connection.onConnectionStateChange((state) => {
            set({
                connectionStatus: {
                    ...get().connectionStatus,
                    connectionState: state as ConnectionStatus["connectionState"],
                    isConnected: state === "connected",
                },
            });
        });

        // エラーハンドラーを設定
        connection.onError((error) => {
            console.error("WebRTC error in game:", error);
            // ユーザーに通知するためのエラー状態を設定
            set({
                connectionStatus: {
                    ...get().connectionStatus,
                    connectionState: "failed",
                    isConnected: false,
                },
            });
            // ユーザーフレンドリーなエラーメッセージを表示
            if (error.getUserMessage) {
                console.error("User-friendly error:", error.getUserMessage());
            }
        });

        // 接続品質ハンドラーを設定
        connection.onQualityChange((quality) => {
            set({ connectionQuality: quality });
        });

        // 接続進捗ハンドラーを設定
        connection.onProgressChange((progress) => {
            set({ connectionProgress: progress });
        });

        const answer = await connection.joinAsGuest(offer);

        set({
            isOnlineGame: true,
            webrtcConnection: connection,
            localPlayer: "white", // ゲストは後手
            localPlayerName: playerName || "",
            connectionStatus: {
                isConnected: false,
                isHost: false,
                peerId: connection.getConnectionInfo().peerId,
                connectionState: "connecting",
            },
        });

        get().resetGame();
        set({ gameMode: "playing" });

        return answer;
    },

    acceptOnlineAnswer: async (answer: string) => {
        const { webrtcConnection } = get();
        if (!webrtcConnection) throw new Error("No connection");

        await webrtcConnection.acceptAnswer(answer);

        // 接続完了後、ゲーム開始メッセージを送信
        const { localPlayerName } = get();
        const gameStartMessage: GameStartMessage = {
            type: "game_start",
            data: {
                hostPlayer: "black",
                guestPlayer: "white",
                playerName: localPlayerName,
            },
            timestamp: Date.now(),
            playerId: webrtcConnection.getConnectionInfo().peerId,
        };

        webrtcConnection.sendMessage(gameStartMessage);
    },

    handleOnlineMessage: (message: GameMessage) => {
        console.log("Received online message:", message);

        if (isMoveMessage(message)) {
            const { data } = message;
            const { board, hands, currentPlayer, localPlayer } = get();

            // 自分の手番でないことを確認（相手の手のみ受け入れる）
            if (currentPlayer === localPlayer) {
                console.warn("Received move during own turn, ignoring");
                return;
            }

            // 受信した手の検証
            const validation = validateReceivedMove(board, hands, currentPlayer, data);
            if (!validation.valid) {
                console.error("Invalid move received:", validation.error);
                // 不正な手を受信した場合の処理
                // TODO: エラーメッセージをUIに表示するか、同期エラーとして処理
                return;
            }

            if (data.drop) {
                // 駒打ちの場合
                const to: Square = {
                    row: Number.parseInt(data.to[0]) as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                    column: Number.parseInt(data.to[1]) as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                };
                get().makeDrop(data.drop, to);
            } else if (data.from) {
                // 通常の移動の場合
                const from: Square = {
                    row: Number.parseInt(data.from[0]) as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                    column: Number.parseInt(data.from[1]) as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                };
                const to: Square = {
                    row: Number.parseInt(data.to[0]) as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                    column: Number.parseInt(data.to[1]) as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9,
                };

                // 相手の手を反映
                get().makeMove(from, to, data.promote);
            }
        } else if (isGameStartMessage(message)) {
            console.log("Game started:", message.data);
            // 相手のプレイヤー名を保存
            if (message.data.playerName) {
                set({ remotePlayerName: message.data.playerName });
            }
            // ゲストも自分の名前を送り返す（初回接続時）
            const { connectionStatus, localPlayerName, webrtcConnection } = get();
            if (!connectionStatus.isHost && webrtcConnection) {
                const responseMessage: GameStartMessage = {
                    type: "game_start",
                    data: {
                        hostPlayer: "black",
                        guestPlayer: "white",
                        playerName: localPlayerName,
                    },
                    timestamp: Date.now(),
                    playerId: webrtcConnection.getConnectionInfo().peerId,
                };
                webrtcConnection.sendMessage(responseMessage);
            }
        } else if (isResignMessage(message)) {
            // 相手が投了した
            const { currentPlayer } = get();
            const winStatus = currentPlayer === "black" ? "black_win" : "white_win";
            set({
                gameStatus: winStatus,
                resignedPlayer: currentPlayer === "black" ? "white" : "black",
            });
            playGameSound("gameEnd");
        } else if (isDrawOfferMessage(message)) {
            // 引き分け提案メッセージの処理
            const { data } = message;
            if (data.accepted === undefined) {
                // 新規提案を受信
                const { currentPlayer, localPlayer } = get();
                const offerer = currentPlayer === localPlayer ? "white" : "black";
                set({ drawOfferPending: true, pendingDrawOfferer: offerer });
            } else if (data.accepted === true) {
                // 承認を受信
                set({
                    gameStatus: "draw",
                    drawOfferPending: false,
                    pendingDrawOfferer: null,
                });
                playGameSound("gameEnd");
            } else {
                // 拒否を受信
                set({
                    drawOfferPending: false,
                    pendingDrawOfferer: null,
                });
            }
        } else if (isTimerConfigMessage(message)) {
            // ホストからタイマー設定を受信（ゲストのみ）
            const { connectionStatus } = get();
            if (!connectionStatus.isHost) {
                get().initializeTimer(message.data);
            }
        } else if (isTimerUpdateMessage(message)) {
            // 相手のタイマー更新を受信
            const { localPlayer } = get();
            const { data } = message;

            // 相手のタイマー情報のみ更新（自分のタイマーは自分で管理）
            set({
                timer: {
                    ...get().timer,
                    blackTime: localPlayer === "white" ? data.blackTime : get().timer.blackTime,
                    whiteTime: localPlayer === "black" ? data.whiteTime : get().timer.whiteTime,
                    blackInByoyomi:
                        localPlayer === "white" ? data.blackInByoyomi : get().timer.blackInByoyomi,
                    whiteInByoyomi:
                        localPlayer === "black" ? data.whiteInByoyomi : get().timer.whiteInByoyomi,
                },
            });
        } else if (isRepetitionCheckMessage(message)) {
            // 千日手チェックメッセージの処理
            const { data } = message;
            if (data.isSennichite) {
                if (data.isPerpetualCheck) {
                    // 連続王手の千日手
                    const { currentPlayer } = get();
                    const winStatus = currentPlayer === "black" ? "white_win" : "black_win";
                    set({ gameStatus: winStatus });
                    console.log("Perpetual check received - winner:", winStatus);
                } else {
                    // 通常の千日手
                    set({ gameStatus: "sennichite" });
                    console.log("Sennichite (repetition) received");
                }
                playGameSound("gameEnd");
            }
        } else if (isJishogiCheckMessage(message)) {
            // 持将棋チェックメッセージの処理
            const { data } = message;
            if (data.isJishogi) {
                set({ gameStatus: "draw" });
                console.log("Jishogi (impasse) received:", data);
                playGameSound("gameEnd");
            }
        } else if (isTryRuleMessage(message)) {
            // トライルールメッセージの処理
            const { data } = message;
            const gameStatus = data.winner === "black" ? "try_rule_black" : "try_rule_white";
            set({ gameStatus });
            console.log("Try rule victory received:", data.winner);
            playGameSound("gameEnd");
        } else if (isStateSyncRequestMessage(message)) {
            // 状態同期リクエストを受信
            const { moveHistory, timer, webrtcConnection } = get();
            if (webrtcConnection) {
                // 現在のゲーム状態を送信
                const syncResponse: StateSyncResponseMessage = {
                    type: "state_sync_response",
                    data: {
                        moveHistory: moveHistory.map((move) => ({
                            from:
                                move.type === "move"
                                    ? (`${move.from.row}${move.from.column}` as SquareKey)
                                    : ("" as SquareKey),
                            to: `${move.to.row}${move.to.column}` as SquareKey,
                            promote: move.type === "move" ? move.promote : undefined,
                            drop: move.type === "drop" ? move.piece.type : undefined,
                        })),
                        currentMoveIndex: moveHistory.length - 1,
                        timerState: timer.config.mode
                            ? {
                                  blackTime: timer.blackTime,
                                  whiteTime: timer.whiteTime,
                                  blackInByoyomi: timer.blackInByoyomi,
                                  whiteInByoyomi: timer.whiteInByoyomi,
                                  activePlayer: timer.activePlayer,
                              }
                            : undefined,
                    },
                    timestamp: Date.now(),
                    playerId: webrtcConnection.getConnectionInfo().peerId,
                };
                webrtcConnection.sendMessage(syncResponse);
                console.log("Sent state sync response");
            }
        } else if (isStateSyncResponseMessage(message)) {
            // 状態同期レスポンスを受信
            console.log("Received state sync response", message.data);
            const { data } = message;

            // 手数が異なる場合のみ同期
            const { moveHistory } = get();
            if (data.currentMoveIndex >= moveHistory.length) {
                console.log("Syncing game state from peer");
                // TODO: 手の履歴からゲーム状態を再構築
                // ここでは基本的な同期ロジックを実装
                // 完全な実装は将来的に拡張
            }

            // タイマー状態の同期
            if (data.timerState) {
                const { localPlayer } = get();
                set({
                    timer: {
                        ...get().timer,
                        blackTime:
                            localPlayer === "white"
                                ? data.timerState.blackTime
                                : get().timer.blackTime,
                        whiteTime:
                            localPlayer === "black"
                                ? data.timerState.whiteTime
                                : get().timer.whiteTime,
                        blackInByoyomi:
                            localPlayer === "white"
                                ? data.timerState.blackInByoyomi
                                : get().timer.blackInByoyomi,
                        whiteInByoyomi:
                            localPlayer === "black"
                                ? data.timerState.whiteInByoyomi
                                : get().timer.whiteInByoyomi,
                    },
                });
            }
        }
    },

    disconnectOnline: () => {
        const { webrtcConnection } = get();
        if (webrtcConnection) {
            webrtcConnection.disconnect();
        }

        set({
            isOnlineGame: false,
            webrtcConnection: null,
            connectionQuality: null,
            connectionProgress: "idle",
            localPlayer: null,
            localPlayerName: "",
            remotePlayerName: "",
            connectionStatus: {
                isConnected: false,
                isHost: false,
                peerId: "",
                connectionState: "new",
            },
        });
    },

    // 詰み探索機能
    startMateSearch: async (options?: Partial<MateSearchOptions>) => {
        const { board, hands, currentPlayer, gameStatus } = get();

        // ゲーム終了状態では探索不可
        if (gameStatus !== "playing" && gameStatus !== "check") {
            return;
        }

        const defaultOptions: MateSearchOptions = {
            maxDepth: 7,
            useWasm: false,
            timeout: 30000,
        };

        const searchOptions = { ...defaultOptions, ...options };

        set({
            mateSearch: {
                status: "searching",
                depth: 1,
                maxDepth: searchOptions.maxDepth,
                result: null,
                error: null,
            },
        });

        try {
            const mateSearchService = new MateSearchService();
            const result = await mateSearchService.search(board, hands, currentPlayer, {
                maxDepth: searchOptions.maxDepth,
                timeout: searchOptions.timeout,
            });

            if (result.isMate) {
                // 手順を棋譜形式に変換（詳細版）
                let tempBoard = structuredClone(board);
                let tempHands = structuredClone(hands);
                let tempPlayer = currentPlayer;

                const moveStrings = result.moves.map((move) => {
                    const turnSymbol = tempPlayer === "black" ? "▲" : "△";
                    let moveString = "";

                    if (move.type === "move") {
                        // 移動する駒の情報を取得
                        const piece = tempBoard[`${move.from.row}${move.from.column}` as SquareKey];
                        if (!piece) return "エラー";

                        // 駒の日本語名を取得
                        const pieceName = getPieceJapaneseName(piece);
                        const fromPos = `${move.from.row}${move.from.column}`;
                        const toPos = `${move.to.row}${move.to.column}`;
                        const promoteText = move.promote ? "成" : "";

                        moveString = `${turnSymbol}${fromPos}${pieceName}→${toPos}${promoteText}`;
                    } else {
                        // 駒の日本語名を取得
                        const pieceName = getPieceJapaneseName(move.piece);
                        const toPos = `${move.to.row}${move.to.column}`;

                        moveString = `${turnSymbol}${pieceName}打${toPos}`;
                    }

                    // 実際に手を適用して盤面を更新
                    const result = applyMove(tempBoard, tempHands, tempPlayer, move);
                    tempBoard = result.board;
                    tempHands = result.hands;
                    tempPlayer = tempPlayer === "black" ? "white" : "black";

                    return moveString;
                });

                set({
                    mateSearch: {
                        status: "found",
                        depth: result.moves.length,
                        maxDepth: searchOptions.maxDepth,
                        result: {
                            isMate: true,
                            moves: moveStrings,
                            nodeCount: result.nodeCount,
                            elapsedMs: result.elapsedMs,
                            depth: result.moves.length,
                        },
                        error: null,
                    },
                });
            } else {
                set({
                    mateSearch: {
                        status: "not_found",
                        depth: searchOptions.maxDepth,
                        maxDepth: searchOptions.maxDepth,
                        result: {
                            isMate: false,
                            moves: [],
                            nodeCount: result.nodeCount,
                            elapsedMs: result.elapsedMs,
                            depth: 0,
                        },
                        error: null,
                    },
                });
            }
        } catch (error) {
            set({
                mateSearch: {
                    status: "error",
                    depth: 0,
                    maxDepth: searchOptions.maxDepth,
                    result: null,
                    error: error instanceof Error ? error.message : "Unknown error",
                },
            });
        }
    },

    cancelMateSearch: () => {
        set({
            mateSearch: {
                status: "idle",
                depth: 0,
                maxDepth: 7,
                result: null,
                error: null,
            },
        });
    },

    // AI対戦機能
    startAIGame: async (
        difficulty: AIDifficulty = "intermediate",
        playerColor: Player = "black",
    ) => {
        const state = get();

        // 既存のAIを停止
        if (state.aiPlayer) {
            state.aiPlayer.dispose();
        }

        // 新しいAIプレイヤーを作成
        const aiPlayer = new AIPlayerService(difficulty);
        await aiPlayer.initialize();

        set({
            gameType: "ai",
            aiPlayer,
            aiPlayerInfo: aiPlayer.getPlayer(),
            aiDifficulty: difficulty,
            localPlayerColor: playerColor,
            gameMode: "playing",
            gameStatus: "playing",
            board: modernInitialBoard,
            hands: structuredClone(initialHands()),
            currentPlayer: "black",
            moveHistory: [],
            historyCursor: HISTORY_CURSOR.LATEST_POSITION,
            selectedSquare: null,
            selectedDropPiece: null,
            validMoves: [],
            validDropSquares: [],
            resignedPlayer: null,
            branchInfo: null,
            originalMoveHistory: [],
            undoStack: [],
        });

        // タイマーを初期化して開始
        if (get().timer.config.mode) {
            get().startPlayerTimer("black");
        }

        // AIの手番の場合はAIに思考させる
        if (playerColor === "white") {
            await get().executeAIMove();
        }
    },

    setAIDifficulty: async (difficulty: AIDifficulty) => {
        const { aiPlayer } = get();
        if (aiPlayer) {
            await aiPlayer.setDifficulty(difficulty);
            set({
                aiDifficulty: difficulty,
                aiPlayerInfo: aiPlayer.getPlayer(),
            });
        }
    },

    stopAI: () => {
        const { aiPlayer } = get();
        if (aiPlayer) {
            aiPlayer.dispose();
            set({
                gameType: "local",
                aiPlayer: null,
                aiPlayerInfo: null,
                isAIThinking: false,
            });
        }
    },

    // AI実行のヘルパー関数
    executeAIMove: async () => {
        const { aiPlayer, board, hands, currentPlayer, gameStatus, localPlayerColor } = get();

        if (
            !aiPlayer ||
            (gameStatus !== "playing" && gameStatus !== "check") ||
            currentPlayer === localPlayerColor
        ) {
            return;
        }

        set({ isAIThinking: true });

        try {
            // AIに手を計算させる
            const move = await aiPlayer.calculateMove(
                board,
                hands,
                currentPlayer,
                get().moveHistory,
            );

            // 少し遅延を入れて人間らしくする
            await new Promise((resolve) => setTimeout(resolve, 300));

            // 手を実行
            if (move.type === "drop") {
                get().makeDrop(move.piece.type, move.to);
            } else {
                get().makeMove(move.from, move.to, move.promote);
            }

            // 念のため、手を実行した後にAI思考フラグが解除されているか確認
            const currentState = get();
            if (currentState.isAIThinking) {
                console.warn("isAIThinking was still true after AI move, resetting it");
                set({ isAIThinking: false });
            }
        } catch (error) {
            console.error("AI move error:", error);
            // エラー時はAI思考フラグを解除
            set({ isAIThinking: false });
        }
    },
}));
