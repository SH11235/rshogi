import type { PieceType, SquareKey } from "shogi-core";

// 基本的なゲームメッセージの型
export interface GameMessage {
    type: "move" | "resign" | "draw_offer" | "game_start" | "sync_state";
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

// 状態同期メッセージ
export interface SyncStateMessage extends GameMessage {
    type: "sync_state";
    data: {
        moveHistory: Array<{
            from: SquareKey;
            to: SquareKey;
            promote?: boolean;
            drop?: PieceType;
        }>;
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

export function isSyncStateMessage(msg: GameMessage): msg is SyncStateMessage {
    return msg.type === "sync_state";
}
