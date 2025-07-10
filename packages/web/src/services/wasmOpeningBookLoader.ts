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
        beginner: `${import.meta.env.BASE_URL}data/opening_book_tournament.binz`,
        intermediate: `${import.meta.env.BASE_URL}data/opening_book_early.binz`,
        advanced: `${import.meta.env.BASE_URL}data/opening_book_standard.binz`,
        expert: `${import.meta.env.BASE_URL}data/opening_book_full.binz`,
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
            console.log(`[WasmOpeningBookLoader] Fetch response headers for ${filePath}:`, {
                "content-type": response.headers.get("content-type"),
                "content-encoding": response.headers.get("content-encoding"),
                "content-length": response.headers.get("content-length"),
                "cache-control": response.headers.get("cache-control"),
            });

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

    // loadFromFallback(): OpeningBookInterface {
    //     // generateMainOpeningsを使ってフォールバックデータを生成
    //     const book = new FallbackOpeningBook();
    //     const entries = generateMainOpenings();

    //     for (const entry of entries) {
    //         book.addEntry(entry);
    //     }

    //     return book;
    // }

    private createOpeningBookFromWasm(): OpeningBookInterface {
        if (!this.reader) {
            throw new Error("WASM reader not initialized");
        }
        // WASM実装をラップしたOpeningBookインスタンスを返す
        return new WasmBackedOpeningBook(this.reader);
    }
}

// 棋譜記法を座標に変換
function convertNotationToMove(
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
        // 文字列（SFEN）が渡された場合とPositionStateが渡された場合の処理
        let sfen: string;
        if (typeof positionOrSfen === "string") {
            sfen = positionOrSfen;
        } else if (positionOrSfen?.board && positionOrSfen.hands && positionOrSfen.currentPlayer) {
            try {
                // 手数を計算（moveHistoryがあればその長さ+1、なければ1）
                const moveNumber = options?.moveHistory ? options.moveHistory.length + 1 : 1;

                sfen = exportToSfen(
                    positionOrSfen.board,
                    positionOrSfen.hands,
                    positionOrSfen.currentPlayer,
                    moveNumber,
                );
                console.log("[Opening Book Debug] Generated SFEN:", sfen);
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

        if (!sfen) {
            console.error("[Opening Book Debug] SFEN is null or undefined");
            return [];
        }

        try {
            const sfenForSearch = sfen.startsWith("sfen ") ? sfen.slice(5) : sfen;
            console.log("[Opening Book Debug] SFEN for search (without prefix):", sfenForSearch);
            const movesJson = this.reader.find_moves(sfenForSearch);
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
            const openingMoves: OpeningMove[] = [];

            for (const moveData of moves) {
                const coords = convertNotationToMove(moveData.notation);
                console.log("[Opening Book Debug] Converted coords:", coords);

                if (coords) {
                    const board = (positionOrSfen as PositionState).board;
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
                            promote: moveData.notation.includes("+") || false,
                            captured:
                                board[`${coords.to.column}${coords.to.row}` as keyof Board] || null,
                        };

                        console.log("[Opening Book Debug] Created move:", move);

                        openingMoves.push({
                            move,
                            notation: moveData.notation,
                            weight: moveData.evaluation || 1,
                            depth: moveData.depth,
                        });
                    } else {
                        console.error(
                            "[Opening Book Debug] No piece found at:",
                            `${coords.from.column}${coords.from.row}`,
                        );
                    }
                }
            }
            return openingMoves;
        } catch (error) {
            console.error("[Opening Book Debug] Error in findMoves:", error);
            return [];
        }
    }

    size(): number {
        // 実装を読み込まれた局面数を返す
        return this.reader.position_count;
    }
}
