import init, { OpeningBookReaderWasm } from "@/wasm/shogi_core";
import {
    type AIDifficulty,
    type Board,
    type Column,
    type FindMovesOptions,
    type Move,
    type OpeningBookInterface,
    type OpeningBookLoaderInterface,
    type OpeningEntry,
    type OpeningMove,
    type PositionState,
    type Row,
    exportToSfen,
    generateMainOpenings,
} from "shogi-core";

/**
 * WASM実装を使った定跡ローダー
 */
export class WasmOpeningBookLoader implements OpeningBookLoaderInterface {
    private reader: OpeningBookReaderWasm | null = null;
    private initialized = false;
    private loadedFiles = new Set<string>();
    private wasmInitialized = false;

    // 難易度ごとのファイルマッピング
    private static readonly DIFFICULTY_FILES: Record<AIDifficulty, string> = {
        beginner: "./data/opening_book_tournament.binz",
        intermediate: "./data/opening_book_early.binz",
        advanced: "./data/opening_book_standard.binz",
        expert: "./data/opening_book_full.binz",
    };

    constructor() {
        // コンストラクタは同期的にするため、初期化は別メソッドで行う
        console.log("[WasmOpeningBookLoader] Constructor called");
    }

    private async ensureWasmInitialized(): Promise<void> {
        if (this.wasmInitialized) return;

        try {
            await init();
            this.wasmInitialized = true;
            console.log("[WasmOpeningBookLoader] WASM initialized successfully");
        } catch (error) {
            console.error("[WasmOpeningBookLoader] Failed to initialize WASM:", error);
            throw error;
        }
    }

    private async initializeReader(): Promise<void> {
        if (this.initialized) return;

        await this.ensureWasmInitialized();

        try {
            this.reader = new OpeningBookReaderWasm();
            this.initialized = true;
            console.log("[WasmOpeningBookLoader] Reader created successfully");
        } catch (error) {
            console.error("[WasmOpeningBookLoader] Failed to create reader:", error);
            this.initialized = false;
            throw error;
        }
    }

    async load(filePath: string): Promise<OpeningBookInterface> {
        // 初期化を確実に行う
        await this.initializeReader();

        if (!this.initialized || !this.reader) {
            throw new Error("Opening book reader not initialized");
        }

        if (this.loadedFiles.has(filePath)) {
            console.log(
                `[WasmOpeningBookLoader] File already loaded: ${filePath}, positions: ${this.reader.position_count}`,
            );
            return this.createOpeningBookFromWasm();
        }

        try {
            const response = await fetch(filePath);
            if (!response.ok) {
                throw new Error(`Failed to fetch: ${response.statusText}`);
            }

            const buffer = await response.arrayBuffer();
            const data = new Uint8Array(buffer);
            console.log(
                `[WasmOpeningBookLoader] Loading file ${filePath}, size: ${data.length} bytes`,
            );

            try {
                const result = this.reader.load_data(data);
                console.log(`[WasmOpeningBookLoader] Load result: ${result}`);

                this.loadedFiles.add(filePath);
                console.log(
                    `[WasmOpeningBookLoader] Successfully loaded ${filePath}, positions: ${this.reader.position_count}`,
                );
            } catch (e) {
                console.error("[WasmOpeningBookLoader] Failed to load data:", e);
                throw new Error(
                    `Failed to load opening book data: ${e instanceof Error ? e.message : String(e)}`,
                );
            }

            return this.createOpeningBookFromWasm();
        } catch (error) {
            throw new Error(
                `Failed to load opening book: ${error instanceof Error ? error.message : "Unknown error"}`,
            );
        }
    }

    async loadForDifficulty(difficulty: AIDifficulty): Promise<OpeningBookInterface> {
        const filePath = WasmOpeningBookLoader.DIFFICULTY_FILES[difficulty];
        return this.load(filePath);
    }

    loadFromFallback(): OpeningBookInterface {
        // generateMainOpeningsを使ってフォールバックデータを生成
        const book = new FallbackOpeningBook();
        const entries = generateMainOpenings();

        for (const entry of entries) {
            book.addEntry(entry);
        }

        return book;
    }

    private createOpeningBookFromWasm(): OpeningBookInterface {
        if (!this.reader) {
            throw new Error("WASM reader not initialized");
        }
        // WASM実装をラップしたOpeningBookインスタンスを返す
        return new WasmBackedOpeningBook(this.reader);
    }
}

/**
 * フォールバック用のOpeningBook実装
 */
class FallbackOpeningBook implements OpeningBookInterface {
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

// 棋譜記法を座標に変換
function convertNotationToMove(
    _sfen: string,
    moveNotation: string,
): { from: { row: Row; column: Column }; to: { row: Row; column: Column } } | null {
    try {
        // 定跡の記法は簡略化されたフォーマット（例: "7g7f"）
        // 変換: 列（数字）+ 行（アルファベット） -> { row, column }
        const fromCol = Number.parseInt(moveNotation[0]) as Column;
        const fromRow = (moveNotation.charCodeAt(1) - "a".charCodeAt(0) + 1) as Row;
        const toCol = Number.parseInt(moveNotation[2]) as Column;
        const toRow = (moveNotation.charCodeAt(3) - "a".charCodeAt(0) + 1) as Row;

        return {
            from: { row: fromRow, column: fromCol },
            to: { row: toRow, column: toCol },
        };
    } catch (error) {
        console.error("Failed to convert notation to move:", moveNotation, error);
        return null;
    }
}

/**
 * WASMリーダーをラップしたOpeningBook実装
 */
class WasmBackedOpeningBook implements OpeningBookInterface {
    constructor(private reader: OpeningBookReaderWasm) {}

    addEntry(_entry: OpeningEntry): boolean {
        // WASM実装では動的な追加はサポートしない
        console.warn("addEntry is not supported in WASM-backed implementation");
        return false;
    }

    findMoves(positionOrSfen: PositionState, options?: FindMovesOptions): OpeningMove[] {
        console.log("[Opening Book Debug] findMoves called");
        console.log("[Opening Book Debug] positionOrSfen type:", typeof positionOrSfen);
        console.log("[Opening Book Debug] positionOrSfen value:", positionOrSfen);

        console.log("[Opening Book Debug] positionOrSfen:", positionOrSfen);
        console.log("[Opening Book Debug] options:", options);

        // 文字列（SFEN）が渡された場合とPositionStateが渡された場合の処理
        let sfen: string;
        if (typeof positionOrSfen === "string") {
            sfen = positionOrSfen;
        } else if (positionOrSfen?.board && positionOrSfen.hands && positionOrSfen.currentPlayer) {
            // PositionState から SFEN を生成
            console.log("[Opening Book Debug] Converting PositionState to SFEN");
            try {
                // デバッグ情報の追加
                console.log("[Opening Book Debug] options:", options);
                console.log(
                    "[Opening Book Debug] moveHistory length:",
                    options?.moveHistory?.length || 0,
                );
                console.log("[Opening Book Debug] currentPlayer:", positionOrSfen.currentPlayer);

                // 手数を計算（moveHistoryがあればその長さ+1、なければ1）
                const moveNumber = options?.moveHistory ? options.moveHistory.length + 1 : 1;
                console.log("[Opening Book Debug] Calculated move number:", moveNumber);

                sfen = exportToSfen(
                    positionOrSfen.board,
                    positionOrSfen.hands,
                    positionOrSfen.currentPlayer,
                    moveNumber,
                );
                console.log("[Opening Book Debug] Generated SFEN:", sfen);
                console.log("[Opening Book Debug] SFEN turn indicator:", sfen.split(" ")[2]); // "b" or "w"
            } catch (conversionError) {
                console.error(
                    "[Opening Book Debug] Failed to convert PositionState to SFEN:",
                    conversionError,
                );
                return [];
            }
        } else {
            console.error(
                "[Opening Book Debug] Invalid input - not a string or PositionState:",
                positionOrSfen,
            );
            return [];
        }

        console.log("[Opening Book Debug] extracted sfen:", sfen);
        console.log("[Opening Book Debug] sfen type:", typeof sfen);
        console.log("[Opening Book Debug] sfen length:", sfen?.length);

        if (!sfen) {
            console.error("[Opening Book Debug] SFEN is null or undefined");
            return [];
        }

        try {
            console.log("[Opening Book Debug] Calling WASM find_moves with sfen:", sfen);

            // Strip "sfen" prefix if present - the Rust hasher expects just the position part
            const sfenForSearch = sfen.startsWith("sfen ") ? sfen.slice(5) : sfen;
            console.log("[Opening Book Debug] SFEN for search (without prefix):", sfenForSearch);

            const movesJson = this.reader.find_moves(sfenForSearch);
            console.log("[Opening Book Debug] WASM find_moves returned:", movesJson);

            const moves = JSON.parse(movesJson);
            console.log("[Opening Book Debug] Parsed moves:", moves);
            console.log("[Opening Book Debug] Number of moves found:", moves?.length || 0);

            if (!moves || moves.length === 0) {
                console.log("[Opening Book Debug] No moves found for position");
                console.log("[Opening Book Debug] Search SFEN was:", sfenForSearch);
                return [];
            }

            // positionOrSfenが文字列の場合はOpeningEntryを返す
            if (typeof positionOrSfen === "string") {
                console.log("[Opening Book Debug] Returning OpeningEntry format");
                return moves;
            }

            // PositionStateの場合はOpeningMove[]を返す
            console.log("[Opening Book Debug] Converting to OpeningMove format");
            const openingMoves: OpeningMove[] = [];

            for (const moveData of moves) {
                console.log("[Opening Book Debug] Processing move:", moveData);
                const coords = convertNotationToMove(sfen, moveData.move);
                console.log("[Opening Book Debug] Converted coords:", coords);

                if (coords) {
                    const board = (positionOrSfen as PositionState).board;
                    console.log("[Opening Book Debug] Board:", board);

                    const piece = board[`${coords.from.column}${coords.from.row}` as keyof Board];
                    console.log(
                        "[Opening Book Debug] Piece at from position:",
                        piece,
                        `at ${coords.from.column}${coords.from.row}`,
                    );

                    if (piece) {
                        const move: Move = {
                            type: "move",
                            from: coords.from,
                            to: coords.to,
                            piece,
                            promote: moveData.move.includes("+") || false,
                            captured:
                                board[`${coords.to.column}${coords.to.row}` as keyof Board] || null,
                        };

                        console.log("[Opening Book Debug] Created move:", move);

                        openingMoves.push({
                            move,
                            notation: moveData.move,
                            weight: moveData.weight || 1,
                            depth: moveData.depth,
                            name: moveData.name,
                            comment: moveData.comment,
                        });
                    } else {
                        console.error(
                            "[Opening Book Debug] No piece found at:",
                            `${coords.from.column}${coords.from.row}`,
                        );
                    }
                }
            }

            console.log("[Opening Book Debug] Final openingMoves:", openingMoves);
            return openingMoves;
        } catch (error) {
            console.error("[Opening Book Debug] Error in findMoves:", error);
            return [];
        }
    }

    clear(): void {
        // WASM実装では必要に応じて実装
        console.warn("clear is not fully supported in WASM-backed implementation");
    }

    size(): number {
        // 実装を読み込まれた局面数を返す
        return this.reader.position_count;
    }

    getMemoryUsage(): number {
        // WASM側で管理されているため、正確な値は取得できない
        return 0;
    }

    getPositionCount(): number {
        return this.reader.position_count;
    }

    selectMove(
        positionOrSfen: PositionState | string,
    ): { move: OpeningMove; weight: number } | null {
        const moves = this.findMoves(
            positionOrSfen as PositionState,
            typeof positionOrSfen === "string" ? undefined : { randomize: false },
        );

        if (!moves || moves.length === 0) {
            return null;
        }

        // 重み付き確率で手を選択
        const totalWeight = moves.reduce(
            (sum: number, move: OpeningMove) => sum + (move.weight || 1),
            0,
        );
        let random = Math.random() * totalWeight;

        for (const move of moves) {
            const weight = move.weight || 1;
            random -= weight;
            if (random <= 0) {
                return { move, weight };
            }
        }

        // フォールバック
        const move = moves[0];
        return { move, weight: move.weight || 1 };
    }
}
