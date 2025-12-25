import type { Player } from "./board";

/**
 * 終局理由
 */
export type GameEndReason =
    | { kind: "time_expired"; loser: Player }
    | { kind: "resignation"; loser: Player }
    | { kind: "checkmate"; loser: Player }
    | { kind: "win_declaration"; winner: Player };

/**
 * 対局結果
 */
export interface GameResult {
    /** 勝者 */
    winner: Player;
    /** 終了理由 */
    reason: GameEndReason;
    /** 終局時点での手数 */
    totalMoves: number;
}

/**
 * 終局理由を日本語で取得
 */
export function getReasonText(reason: GameEndReason): string {
    const loserLabel = (player: Player) => (player === "sente" ? "先手" : "後手");
    const winnerLabel = (player: Player) => (player === "sente" ? "先手" : "後手");

    switch (reason.kind) {
        case "time_expired":
            return `${loserLabel(reason.loser)}が時間切れ`;
        case "resignation":
            return `${loserLabel(reason.loser)}が投了`;
        case "checkmate":
            return `${loserLabel(reason.loser)}が詰み`;
        case "win_declaration":
            return `${winnerLabel(reason.winner)}が勝利宣言`;
    }
}

/**
 * 勝者ラベルを取得
 */
export function getWinnerLabel(winner: Player): string {
    return winner === "sente" ? "先手の勝ち" : "後手の勝ち";
}
