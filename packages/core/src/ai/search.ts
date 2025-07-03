import type { Board } from "../domain/model/board";
import type { Move } from "../domain/model/move";
import type { Player } from "../domain/model/piece";
import { isCheckmate, isInCheck } from "../domain/service/checkmate";
import type { Hands } from "../domain/service/moveService";
import { applyMove } from "../domain/service/moveService";
import type { SearchOptions, SearchResult, TranspositionEntry } from "../types/ai";

// 置換表（Transposition Table）
export class TranspositionTable {
    private table: Map<string, TranspositionEntry>;
    private maxSize: number;

    constructor(maxSize = 1000000) {
        this.table = new Map();
        this.maxSize = maxSize;
    }

    // 局面のハッシュキーを生成
    private getKey(board: Board, hands: Hands, player: Player): string {
        // 簡易的なハッシュキー生成（実際の実装ではZobristハッシュを使用すべき）
        return JSON.stringify({ board, hands, player });
    }

    // エントリを取得
    get(board: Board, hands: Hands, player: Player): TranspositionEntry | undefined {
        const key = this.getKey(board, hands, player);
        return this.table.get(key);
    }

    // エントリを保存
    set(board: Board, hands: Hands, player: Player, entry: TranspositionEntry): void {
        if (this.table.size >= this.maxSize) {
            // 最も古いエントリを削除（簡易的な実装）
            const firstKey = this.table.keys().next().value;
            if (firstKey) {
                this.table.delete(firstKey);
            }
        }

        const key = this.getKey(board, hands, player);
        this.table.set(key, entry);
    }

    clear(): void {
        this.table.clear();
    }
}

// ムーブオーダリング（手の優先順位付け）
export function orderMoves(
    moves: Move[],
    board: Board,
    hands: Hands,
    player: Player,
    pvMove?: Move,
    killerMoves?: Move[],
): Move[] {
    // 各手にスコアを付与
    const scoredMoves = moves.map((move) => {
        let score = 0;

        // PV（主要変化）の手は最優先
        if (pvMove && movesEqual(move, pvMove)) {
            score += 10000;
        }

        // キラームーブ（同じ深さで良かった手）
        if (killerMoves) {
            const killerIndex = killerMoves.findIndex((km) => movesEqual(move, km));
            if (killerIndex >= 0) {
                score += 8000 - killerIndex * 100;
            }
        }

        // 駒を取る手は優先
        if (move.type === "move" && move.captured) {
            // MVV-LVA（Most Valuable Victim - Least Valuable Attacker）
            const victimValue = getPieceValue(move.captured);
            const attackerValue = getPieceValue(move.piece);
            score += 5000 + victimValue - attackerValue / 10;
        }

        // 成る手は優先
        if (move.type === "move" && move.promote) {
            score += 1000;
        }

        // 王手は優先（エラー処理を追加）
        try {
            const newBoard = applyMove(board, hands, player, move);
            const opponent = player === "black" ? "white" : "black";
            if (isInCheck(newBoard.board, opponent)) {
                score += 2000;
            }
        } catch (e) {
            // 無効な手の場合は無視
        }

        // 中央への移動を優先
        if (move.type === "move" || move.type === "drop") {
            const centerDistance = Math.abs(move.to.row - 5) + Math.abs(move.to.column - 5);
            score += (8 - centerDistance) * 10;
        }

        return { move, score };
    });

    // スコアでソート
    scoredMoves.sort((a, b) => b.score - a.score);

    return scoredMoves.map((sm) => sm.move);
}

// 手の比較
function movesEqual(move1: Move, move2: Move): boolean {
    if (move1.type !== move2.type) return false;

    if (move1.type === "move" && move2.type === "move") {
        return (
            move1.from.row === move2.from.row &&
            move1.from.column === move2.from.column &&
            move1.to.row === move2.to.row &&
            move1.to.column === move2.to.column &&
            move1.promote === move2.promote
        );
    }

    if (move1.type === "drop" && move2.type === "drop") {
        return (
            move1.piece.type === move2.piece.type &&
            move1.to.row === move2.to.row &&
            move1.to.column === move2.to.column
        );
    }

    return false;
}

// 駒の価値（簡易版）
function getPieceValue(piece: { type: string }): number {
    const values: Record<string, number> = {
        pawn: 100,
        lance: 430,
        knight: 450,
        silver: 640,
        gold: 690,
        bishop: 890,
        rook: 1040,
        king: 10000,
        gyoku: 10000,
    };
    return values[piece.type] || 0;
}

// 反復深化探索
export class IterativeDeepeningSearch {
    private transpositionTable: TranspositionTable;
    private killerMoves: Map<number, Move[]>; // 深さごとのキラームーブ
    private pvTable: Map<number, Move>; // 主要変化テーブル
    private nodesSearched: number;
    private shouldStop: boolean;

    constructor() {
        this.transpositionTable = new TranspositionTable();
        this.killerMoves = new Map();
        this.pvTable = new Map();
        this.nodesSearched = 0;
        this.shouldStop = false;
    }

    // 反復深化探索のメインメソッド
    async search(
        board: Board,
        hands: Hands,
        player: Player,
        moves: Move[],
        options: SearchOptions,
    ): Promise<SearchResult> {
        this.nodesSearched = 0;
        this.shouldStop = false;
        this.killerMoves.clear();
        this.pvTable.clear();

        const startTime = Date.now();
        let bestMove = moves[0];
        let bestScore = Number.NEGATIVE_INFINITY;
        let completedDepth = 0;
        const pv: Move[] = [];

        // 反復深化：1手から最大探索深度まで段階的に深くする
        for (let depth = 1; depth <= options.maxDepth; depth++) {
            if (this.shouldStop || Date.now() - startTime > options.timeLimit) {
                break;
            }

            // 前回の最善手を最初に探索
            const pvMove = this.pvTable.get(0);
            const orderedMoves = orderMoves(
                moves,
                board,
                hands,
                player,
                pvMove,
                this.killerMoves.get(0),
            );

            let currentBestMove = orderedMoves[0];
            let currentBestScore = Number.NEGATIVE_INFINITY;
            let currentPv: Move[] = [];

            // 各手を探索
            for (const move of orderedMoves) {
                if (this.shouldStop || Date.now() - startTime > options.timeLimit) {
                    break;
                }

                const newBoard = applyMove(board, hands, player, move);
                const score = -this.alphaBeta(
                    newBoard.board,
                    newBoard.hands,
                    player === "black" ? "white" : "black",
                    depth - 1,
                    Number.NEGATIVE_INFINITY,
                    -currentBestScore,
                    1,
                    options,
                );

                if (score > currentBestScore) {
                    currentBestScore = score;
                    currentBestMove = move;
                    currentPv = [move];

                    // 主要変化を更新
                    this.pvTable.set(0, move);
                }
            }

            // この深さでの探索が完了したら結果を更新
            if (!this.shouldStop && Date.now() - startTime <= options.timeLimit) {
                bestMove = currentBestMove;
                bestScore = currentBestScore;
                completedDepth = depth;
                pv.splice(0, pv.length, ...currentPv);
            }
        }

        return {
            bestMove,
            score: bestScore,
            depth: completedDepth,
            pv,
            nodes: this.nodesSearched,
            time: Date.now() - startTime,
        };
    }

    // Alpha-Beta探索
    private alphaBeta(
        board: Board,
        hands: Hands,
        player: Player,
        depth: number,
        initialAlpha: number,
        initialBeta: number,
        ply: number,
        options: SearchOptions,
    ): number {
        this.nodesSearched++;

        let alpha = initialAlpha;
        let beta = initialBeta;

        // 置換表を確認
        const ttEntry = this.transpositionTable.get(board, hands, player);
        if (ttEntry && ttEntry.depth >= depth) {
            if (ttEntry.type === "exact") {
                return ttEntry.score;
            }
            if (ttEntry.type === "lowerbound") {
                alpha = Math.max(alpha, ttEntry.score);
            }
            if (ttEntry.type === "upperbound") {
                beta = Math.min(beta, ttEntry.score);
            }

            if (alpha >= beta) {
                return ttEntry.score;
            }
        }

        // 終端ノード評価
        if (depth === 0) {
            const score = options.evaluate(board, hands, player);
            return score;
        }

        // 詰みチェック
        if (isCheckmate(board, hands, player)) {
            return -100000 + ply; // 早い詰みを優先
        }

        // 手の生成とオーダリング
        const moves = options.generateMoves(board, hands, player);
        if (moves.length === 0) {
            return 0; // ステイルメイト
        }

        const pvMove = this.pvTable.get(ply);
        const orderedMoves = orderMoves(
            moves,
            board,
            hands,
            player,
            pvMove,
            this.killerMoves.get(ply),
        );

        let bestScore = Number.NEGATIVE_INFINITY;
        let bestMove: Move | null = null;
        let ttType: "exact" | "lowerbound" | "upperbound" = "upperbound";

        for (const move of orderedMoves) {
            const newBoard = applyMove(board, hands, player, move);
            const score = -this.alphaBeta(
                newBoard.board,
                newBoard.hands,
                player === "black" ? "white" : "black",
                depth - 1,
                -beta,
                -alpha,
                ply + 1,
                options,
            );

            if (score > bestScore) {
                bestScore = score;
                bestMove = move;
            }

            if (score > alpha) {
                alpha = score;
                ttType = "exact";

                // 主要変化を更新
                if (bestMove) {
                    this.pvTable.set(ply, bestMove);
                }
            }

            if (alpha >= beta) {
                // ベータカット
                ttType = "lowerbound";

                // キラームーブを記録
                if (bestMove && bestMove.type === "move" && !bestMove.captured) {
                    this.addKillerMove(ply, bestMove);
                }
                break;
            }
        }

        // 置換表に保存
        this.transpositionTable.set(board, hands, player, {
            score: bestScore,
            depth,
            type: ttType,
            bestMove: bestMove || undefined,
        });

        return bestScore;
    }

    // キラームーブを追加
    private addKillerMove(ply: number, move: Move): void {
        const killers = this.killerMoves.get(ply) || [];

        // 既に存在する場合は削除
        const index = killers.findIndex((km) => movesEqual(km, move));
        if (index >= 0) {
            killers.splice(index, 1);
        }

        // 先頭に追加
        killers.unshift(move);

        // 最大2つまで保持
        if (killers.length > 2) {
            killers.pop();
        }

        this.killerMoves.set(ply, killers);
    }

    stop(): void {
        this.shouldStop = true;
    }
}
