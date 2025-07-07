import type { Move } from "../domain/model/move";
import type { PositionState } from "../domain/service/repetitionService";

export interface OpeningMove {
    move: Move;
    notation: string;
    weight: number;
    depth?: number;
    name?: string;
    comment?: string;
}

export interface OpeningEntry {
    position: string; // SFEN形式（手数なし）
    moves: OpeningMove[];
    depth: number;
}

export interface FindMovesOptions {
    randomize?: boolean;
    moveHistory?: Move[];
}

export class OpeningBook {
    private positions: Map<string, OpeningMove[]> = new Map();
    private memoryUsage = 0;
    private readonly maxMemory: number;

    constructor(maxMemory: number = 200 * 1024 * 1024) {
        // 200MB default
        this.maxMemory = maxMemory;
    }

    addEntry(entry: OpeningEntry): boolean {
        const entrySize = this.estimateEntrySize(entry);

        if (this.memoryUsage + entrySize > this.maxMemory) {
            return false; // メモリ制限超過
        }

        this.positions.set(entry.position, entry.moves);
        this.memoryUsage += entrySize;

        return true;
    }

    findMoves(position: PositionState, options: FindMovesOptions = {}): OpeningMove[] {
        const key = this.generatePositionKey(position);
        const moves = this.positions.get(key);

        if (!moves || moves.length === 0) {
            return [];
        }

        if (options.randomize) {
            return [this.selectWeightedRandom(moves)];
        }

        // デフォルトは重み順でソート
        return [...moves].sort((a, b) => b.weight - a.weight);
    }

    getMemoryUsage(): number {
        return this.memoryUsage;
    }

    size(): number {
        return this.positions.size;
    }

    clear(): void {
        this.positions.clear();
        this.memoryUsage = 0;
    }

    private estimateEntrySize(entry: OpeningEntry): number {
        // 概算でメモリ使用量を計算
        let size = 0;

        // position文字列
        size += entry.position.length * 2; // UTF-16

        // moves配列
        for (const move of entry.moves) {
            size += 50; // Moveオブジェクトの概算サイズ（より現実的に）
            size += move.notation.length * 2;
            size += 8; // weight (number)
            size += move.depth ? 8 : 0;
            size += move.name ? move.name.length * 2 : 0;
            size += move.comment ? move.comment.length * 2 : 0;
        }

        size += 8; // depth
        size += 20; // オーバーヘッド

        return size;
    }

    private selectWeightedRandom(moves: OpeningMove[]): OpeningMove {
        const totalWeight = moves.reduce((sum, move) => sum + move.weight, 0);
        const random = Math.random() * totalWeight;

        let accumulatedWeight = 0;
        for (const move of moves) {
            accumulatedWeight += move.weight;
            if (random < accumulatedWeight) {
                return move;
            }
        }

        // fallback（通常は到達しない）
        return moves[0];
    }

    private generatePositionKey(_position: PositionState): string {
        // TODO: Implement proper position key generation
        // For now, return a placeholder to avoid build errors
        return "placeholder";
    }
}
