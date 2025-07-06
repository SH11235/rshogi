import type { OpeningBook } from "./openingBook";

/**
 * 定跡ローダーの抽象インターフェース
 * 実装はWeb側でWASMモジュールを使って提供される
 */
export interface OpeningBookLoaderInterface {
    /**
     * 定跡データを読み込む
     */
    load(filePath: string): Promise<OpeningBook>;

    /**
     * 難易度に応じた定跡ファイルを読み込む
     */
    loadForDifficulty(
        difficulty: "beginner" | "intermediate" | "advanced" | "expert",
    ): Promise<OpeningBook>;

    /**
     * フォールバック用の定跡を読み込む
     */
    loadFromFallback(): OpeningBook;
}
