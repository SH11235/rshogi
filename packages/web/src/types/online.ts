import type { PieceType, SquareKey } from "shogi-core";

// 基本的なゲームメッセージの型
export interface GameMessage {
    type:
        | "move"
        | "resign"
        | "draw_offer"
        | "game_start"
        | "timer_config"
        | "timer_update"
        | "repetition_check"
        | "jishogi_check"
        | "spectator_join"
        | "spectator_leave"
        | "spectator_sync";
    data: unknown;
    timestamp: number;
    playerId: string;
}

// 指し手メッセージ
export interface MoveMessage extends GameMessage {
    type: "move";
    data: {
        from: SquareKey;
        to: SquareKey;
        promote?: boolean;
        drop?: PieceType;
    };
}

// ゲーム開始メッセージ
export interface GameStartMessage extends GameMessage {
    type: "game_start";
    data: {
        hostPlayer: "black" | "white";
        guestPlayer: "black" | "white";
    };
}

// 投了メッセージ
export interface ResignMessage extends GameMessage {
    type: "resign";
    data: null;
}

// 引き分け提案メッセージ
export interface DrawOfferMessage extends GameMessage {
    type: "draw_offer";
    data: {
        accepted?: boolean;
    };
}

// タイマー設定メッセージ
export interface TimerConfigMessage extends GameMessage {
    type: "timer_config";
    data: {
        mode: "basic" | "fischer" | "perMove" | null;
        basicTime: number;
        byoyomiTime: number;
        fischerIncrement: number;
        perMoveLimit: number;
    };
}

// タイマー更新メッセージ
export interface TimerUpdateMessage extends GameMessage {
    type: "timer_update";
    data: {
        blackTime: number;
        whiteTime: number;
        blackInByoyomi: boolean;
        whiteInByoyomi: boolean;
        activePlayer: "black" | "white" | null;
    };
}

// 千日手チェックメッセージ
export interface RepetitionCheckMessage extends GameMessage {
    type: "repetition_check";
    data: {
        isSennichite: boolean;
        isPerpetualCheck: boolean;
    };
}

// 持将棋チェックメッセージ
export interface JishogiCheckMessage extends GameMessage {
    type: "jishogi_check";
    data: {
        isJishogi: boolean;
        blackPoints: number;
        whitePoints: number;
    };
}

// 観戦者参加メッセージ
export interface SpectatorJoinMessage extends GameMessage {
    type: "spectator_join";
    data: {
        spectatorId: string;
        spectatorName?: string;
    };
}

// 観戦者退出メッセージ
export interface SpectatorLeaveMessage extends GameMessage {
    type: "spectator_leave";
    data: {
        spectatorId: string;
    };
}

// 観戦者同期メッセージ
export interface SpectatorSyncMessage extends GameMessage {
    type: "spectator_sync";
    data: {
        board: Record<string, unknown>; // Board型のシリアライズデータ
        hands: Record<string, unknown>; // Hands型のシリアライズデータ
        currentPlayer: "black" | "white";
        moveHistory: Array<{
            from: SquareKey;
            to: SquareKey;
            promote?: boolean;
            drop?: PieceType;
        }>;
        gameStatus: string;
        timer?: {
            blackTime: number;
            whiteTime: number;
            blackInByoyomi: boolean;
            whiteInByoyomi: boolean;
        };
    };
}

// 接続状態
export interface ConnectionStatus {
    isConnected: boolean;
    isHost: boolean;
    peerId: string;
    connectionState: "new" | "connecting" | "connected" | "disconnected" | "failed";
}

// メッセージのタイプガード
export function isMoveMessage(msg: GameMessage): msg is MoveMessage {
    return msg.type === "move";
}

export function isGameStartMessage(msg: GameMessage): msg is GameStartMessage {
    return msg.type === "game_start";
}

export function isResignMessage(msg: GameMessage): msg is ResignMessage {
    return msg.type === "resign";
}

export function isDrawOfferMessage(msg: GameMessage): msg is DrawOfferMessage {
    return msg.type === "draw_offer";
}

export function isTimerConfigMessage(msg: GameMessage): msg is TimerConfigMessage {
    return msg.type === "timer_config";
}

export function isTimerUpdateMessage(msg: GameMessage): msg is TimerUpdateMessage {
    return msg.type === "timer_update";
}

export function isRepetitionCheckMessage(msg: GameMessage): msg is RepetitionCheckMessage {
    return msg.type === "repetition_check";
}

export function isJishogiCheckMessage(msg: GameMessage): msg is JishogiCheckMessage {
    return msg.type === "jishogi_check";
}

export function isSpectatorJoinMessage(msg: GameMessage): msg is SpectatorJoinMessage {
    return msg.type === "spectator_join";
}

export function isSpectatorLeaveMessage(msg: GameMessage): msg is SpectatorLeaveMessage {
    return msg.type === "spectator_leave";
}

export function isSpectatorSyncMessage(msg: GameMessage): msg is SpectatorSyncMessage {
    return msg.type === "spectator_sync";
}
