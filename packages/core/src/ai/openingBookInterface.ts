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

export interface OpeningBookInterface {
    addEntry(entry: OpeningEntry): boolean;
    findMoves(position: PositionState, options: FindMovesOptions): OpeningMove[];
    getMemoryUsage(): number;
    size(): number;
    clear(): void;
}

/**
 * 定跡ローダーの抽象インターフェース
 * 実装はWeb側でWASMモジュールを使って提供される
 */
export interface OpeningBookLoaderInterface {
    /**
     * 定跡データを読み込む
     */
    load(filePath: string): Promise<OpeningBookInterface>;

    /**
     * 難易度に応じた定跡ファイルを読み込む
     */
    loadForDifficulty(
        difficulty: "beginner" | "intermediate" | "advanced" | "expert",
    ): Promise<OpeningBookInterface>;

    /**
     * フォールバック用の定跡を読み込む
     */
    loadFromFallback(): OpeningBookInterface;
}
