import {
    OpeningBook,
    type OpeningBookLoaderInterface,
    type OpeningEntry,
    type AIDifficulty,
    generateMainOpenings,
    exportToSfen,
    type Move,
    type Board,
    type Column,
    type Row,
} from "shogi-core";
import { OpeningBookReaderWasm } from "@/wasm/shogi_core";

/**
 * WASM実装を使った定跡ローダー
 */
export class WasmOpeningBookLoader implements OpeningBookLoaderInterface {
    private reader: OpeningBookReaderWasm | null = null;
    private initialized = false;
    private loadedFiles = new Set<string>();

    // 難易度ごとのファイルマッピング
    private static readonly DIFFICULTY_FILES: Record<AIDifficulty, string> = {
        beginner: "/data/opening_book_tournament.binz",
        intermediate: "/data/opening_book_early.binz",
        advanced: "/data/opening_book_standard.binz",
        expert: "/data/opening_book_full.binz",
    };

    private async initialize(): Promise<void> {
        if (this.initialized) return;

        try {
            // WebWorker環境でのWASMモジュール使用可能性をチェック
            console.log("[WASM Debug] Checking WASM availability in Worker environment");
            console.log("[WASM Debug] OpeningBookReaderWasm:", typeof OpeningBookReaderWasm);

            if (typeof OpeningBookReaderWasm === "undefined") {
                throw new Error("OpeningBookReaderWasm is not available in this environment");
            }

            // WASMモジュールが適切に初期化されているかチェック
            try {
                this.reader = new OpeningBookReaderWasm();
                console.log("[WASM Debug] WASM reader instance created successfully");
            } catch (constructorError) {
                console.error(
                    "[WASM Debug] Failed to create WASM reader instance:",
                    constructorError,
                );
                throw new Error(
                    `WASM reader construction failed: ${constructorError instanceof Error ? constructorError.message : "Unknown constructor error"}`,
                );
            }

            this.initialized = true;
            console.log("[WASM Debug] WASM module initialized successfully");
        } catch (error) {
            this.initialized = false;
            console.error("[WASM Debug] Failed to initialize WASM module:", error);
            throw new Error(
                `Failed to initialize WASM module: ${error instanceof Error ? error.message : "Unknown error"}`,
            );
        }
    }

    async load(filePath: string): Promise<OpeningBook> {
        await this.initialize();

        if (!this.reader) {
            throw new Error("WASM reader not initialized");
        }

        // 既に読み込まれている場合はスキップ
        if (this.loadedFiles.has(filePath)) {
            return this.createOpeningBookFromWasm();
        }

        try {
            // ファイルをダウンロード
            const response = await fetch(filePath);
            if (!response.ok) {
                throw new Error(`HTTP ${response.status}`);
            }

            const arrayBuffer = await response.arrayBuffer();
            const data = new Uint8Array(arrayBuffer);

            // WASMに読み込む
            const result = this.reader.load_data(data);
            console.log(`OpeningBook loaded via WASM: ${result}`);

            this.loadedFiles.add(filePath);
            return this.createOpeningBookFromWasm();
        } catch (error) {
            throw new Error(
                `Failed to load opening book: ${error instanceof Error ? error.message : "Unknown error"}`,
            );
        }
    }

    async loadForDifficulty(difficulty: AIDifficulty): Promise<OpeningBook> {
        const filePath = WasmOpeningBookLoader.DIFFICULTY_FILES[difficulty];
        return this.load(filePath);
    }

    loadFromFallback(): OpeningBook {
        // generateMainOpeningsを使ってフォールバックデータを生成
        const book = new OpeningBook();
        const entries = generateMainOpenings();

        for (const entry of entries) {
            book.addEntry(entry);
        }

        return book;
    }

    /**
     * WASMリーダーから OpeningBook インスタンスを作成
     */
    private createOpeningBookFromWasm(): OpeningBook {
        if (!this.reader) {
            throw new Error("WASM reader not initialized");
        }

        // カスタムOpeningBookを作成（WASMリーダーをラップ）
        const book = new WasmBackedOpeningBook(this.reader);
        return book;
    }
}

/**
 * 将棋の記法文字列（例: "2g2f"）をMove型に変換
 */
function convertNotationToMove(
    notation: string,
    board: Board,
    _currentPlayer: "black" | "white",
): Move | null {
    if (!notation || notation.length < 4) return null;

    try {
        // 通常の移動（例: "2g2f"）
        const fromCol = Number.parseInt(notation[0]) as Column;
        const fromRowChar = notation[1];
        const toCol = Number.parseInt(notation[2]) as Column;
        const toRowChar = notation[3];

        // 行文字（a=1, b=2, ..., i=9）を数値に変換
        const fromRow = (fromRowChar.charCodeAt(0) - 96) as Row;
        const toRow = (toRowChar.charCodeAt(0) - 96) as Row;

        if (
            fromCol < 1 ||
            fromCol > 9 ||
            fromRow < 1 ||
            fromRow > 9 ||
            toCol < 1 ||
            toCol > 9 ||
            toRow < 1 ||
            toRow > 9
        ) {
            return null;
        }

        const from = { row: fromRow, column: fromCol };
        const to = { row: toRow, column: toCol };

        // 移動元の駒を取得
        const piece = board[`${fromRow}${fromCol}`];
        if (!piece) return null;

        // 移動先の駒（取る駒）を取得
        const captured = board[`${toRow}${toCol}`] || null;

        // 成りの判定（記法に+が含まれているかチェック）
        const promote = notation.includes("+");

        return {
            type: "move",
            from,
            to,
            piece,
            promote,
            captured,
        };
    } catch (error) {
        console.error("Failed to convert notation to move:", notation, error);
        return null;
    }
}

/**
 * WASMリーダーをラップしたOpeningBook実装
 */
class WasmBackedOpeningBook extends OpeningBook {
    constructor(private reader: OpeningBookReaderWasm) {
        super(200 * 1024 * 1024); // 200MB
    }

    addEntry(_entry: OpeningEntry): boolean {
        // WASM実装では動的な追加はサポートしない
        console.warn("addEntry is not supported in WASM-backed implementation");
        return false;
    }

    findMoves(positionOrSfen: any, options?: any): any {
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
                return typeof positionOrSfen === "string" ? null : [];
            }
        } else {
            console.error(
                "[Opening Book Debug] Invalid input - not a string or PositionState:",
                positionOrSfen,
            );
            return typeof positionOrSfen === "string" ? null : [];
        }

        console.log("[Opening Book Debug] extracted sfen:", sfen);
        console.log("[Opening Book Debug] sfen type:", typeof sfen);
        console.log("[Opening Book Debug] sfen length:", sfen?.length);

        if (!sfen) {
            console.error("[Opening Book Debug] SFEN is null or undefined");
            return typeof positionOrSfen === "string" ? null : [];
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
                return positionOrSfen === sfen ? null : [];
            }

            // positionOrSfenが文字列の場合はOpeningEntryを返す
            if (typeof positionOrSfen === "string") {
                const result = {
                    position: sfen,
                    moves: moves,
                    depth: moves[0]?.depth || 0,
                };
                console.log("[Opening Book Debug] Returning OpeningEntry:", result);
                return result;
            }

            // PositionStateが渡された場合はOpeningMove[]を返す
            // BookMove形式からAI Engineが期待する形式に変換
            const convertedMoves = [];
            for (const bookMove of moves) {
                const move = convertNotationToMove(
                    bookMove.notation,
                    positionOrSfen.board,
                    positionOrSfen.currentPlayer,
                );
                if (move) {
                    convertedMoves.push({
                        move,
                        notation: bookMove.notation,
                        weight: bookMove.evaluation || 1,
                        depth: bookMove.depth || 0,
                    });
                } else {
                    console.warn(
                        "[Opening Book Debug] Failed to convert notation:",
                        bookMove.notation,
                    );
                }
            }

            console.log("[Opening Book Debug] Converted moves:", convertedMoves);
            return convertedMoves;
        } catch (error) {
            console.error("[Opening Book Debug] Failed to find moves:", error);
            console.error("[Opening Book Debug] Error details:", {
                message: error instanceof Error ? error.message : "Unknown error",
                stack: error instanceof Error ? error.stack : undefined,
                sfen: sfen,
                sfenType: typeof sfen,
            });
            return typeof positionOrSfen === "string" ? null : [];
        }
    }

    getMemoryUsage(): number {
        // WASM側で管理されているため、正確な値は取得できない
        return 0;
    }

    getPositionCount(): number {
        return this.reader.position_count;
    }

    selectMove(positionOrSfen: any): { move: any; weight: number } | null {
        const moves = Array.isArray(this.findMoves(positionOrSfen))
            ? this.findMoves(positionOrSfen)
            : this.findMoves(positionOrSfen)?.moves || [];

        if (!moves || moves.length === 0) {
            return null;
        }

        // 重み付き確率で手を選択
        const totalWeight = moves.reduce((sum: number, move: any) => sum + (move.weight || 1), 0);
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
