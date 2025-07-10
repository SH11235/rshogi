import init, { OpeningBookReaderWasm } from "@/wasm/shogi_core";
import {
    type AIDifficulty,
    type Board,
    type Column,
    type FindMovesOptions,
    type Move,
    type OpeningBookInterface,
    type OpeningBookLoaderInterface,
    type OpeningMove,
    type PositionState,
    type Row,
    exportToSfen,
} from "shogi-core";

// ===========================================
// 型定義と定数
// ===========================================

// 難易度ごとのファイルマッピング
const DIFFICULTY_FILES: Record<AIDifficulty, string> = {
    beginner: `${import.meta.env.BASE_URL}data/opening_book_tournament.binz`,
    intermediate: `${import.meta.env.BASE_URL}data/opening_book_early.binz`,
    advanced: `${import.meta.env.BASE_URL}data/opening_book_standard.binz`,
    expert: `${import.meta.env.BASE_URL}data/opening_book_full.binz`,
};

// WASMから返される定跡データの型
interface WasmMoveData {
    notation: string;
    evaluation: number;
    depth: number;
}

// ログレベルの型
type LogLevel = "debug" | "info" | "warn" | "error";

// ===========================================
// ユーティリティ関数
// ===========================================

// ロガーファクトリー（環境に応じてログレベルを制御）
const createLogger = (prefix: string, enabled = true) => {
    const log = (level: LogLevel, message: string, ...args: unknown[]) => {
        if (!enabled) return;

        const methods: Record<LogLevel, typeof console.log> = {
            debug: console.log,
            info: console.log,
            warn: console.warn,
            error: console.error,
        };

        methods[level](`[${prefix}] ${message}`, ...args);
    };

    return {
        debug: (message: string, ...args: unknown[]) => log("debug", message, ...args),
        info: (message: string, ...args: unknown[]) => log("info", message, ...args),
        warn: (message: string, ...args: unknown[]) => log("warn", message, ...args),
        error: (message: string, ...args: unknown[]) => log("error", message, ...args),
    };
};

// 座標のキー作成
export const squareToKey = (row: Row, column: Column): keyof Board => {
    return `${column}${row}` as keyof Board;
};

// 棋譜記法のパース
export const parseNotation = (
    notation: string,
): {
    fromCol: Column;
    fromRow: Row;
    toCol: Column;
    toRow: Row;
    promote: boolean;
} | null => {
    try {
        // 定跡の記法: "7g7f" or "7g7f+"
        const baseNotation = notation.replace("+", "");

        if (baseNotation.length !== 4) {
            return null;
        }

        const fromCol = Number.parseInt(baseNotation[0]) as Column;
        const fromRow = (baseNotation.charCodeAt(1) - "a".charCodeAt(0) + 1) as Row;
        const toCol = Number.parseInt(baseNotation[2]) as Column;
        const toRow = (baseNotation.charCodeAt(3) - "a".charCodeAt(0) + 1) as Row;

        // 範囲チェック
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

        return {
            fromCol,
            fromRow,
            toCol,
            toRow,
            promote: notation.includes("+"),
        };
    } catch {
        return null;
    }
};

// ===========================================
// WASM管理
// ===========================================

// WASM管理のインターフェース
export interface WasmManager {
    initialize(): Promise<void>;
}

/**
 * WASM初期化マネージャーを作成する
 * 各インスタンスは独立した初期化状態を持つ
 */
export const createWasmManager = (): WasmManager => {
    // インスタンスごとの状態をクロージャーで管理
    let initialized = false;
    let initPromise: Promise<void> | null = null;

    const initialize = async (): Promise<void> => {
        if (initialized) return;

        if (initPromise) {
            return initPromise;
        }

        initPromise = init().then(() => {
            initialized = true;
        });

        return initPromise;
    };

    return { initialize };
};

/**
 * 共有WASMマネージャーのファクトリー
 * アプリケーション全体で単一のWASMインスタンスを共有する場合に使用
 */
export const createSharedWasmManager = (() => {
    // このクロージャー内でのみ共有される状態
    let sharedManager: WasmManager | null = null;

    return (): WasmManager => {
        if (!sharedManager) {
            sharedManager = createWasmManager();
        }
        return sharedManager;
    };
})();

/**
 * デフォルトのWasmManagerインスタンスを取得
 * 後方互換性のため維持
 */
export const getDefaultWasmManager = createSharedWasmManager;

// ===========================================
// データ読み込み
// ===========================================

// ファイル読み込み関数
export const fetchOpeningBookData = async (
    filePath: string,
    logger = createLogger("Fetch", false),
): Promise<Uint8Array> => {
    logger.info(`Fetching file: ${filePath}`);

    const response = await fetch(filePath);

    if (!response.ok) {
        throw new Error(`Failed to fetch file: ${response.status} ${response.statusText}`);
    }

    const buffer = await response.arrayBuffer();
    const bytes = new Uint8Array(buffer);

    logger.info(`Fetched ${bytes.length} bytes from ${filePath}`);

    return bytes;
};

// ===========================================
// 定跡データ変換
// ===========================================

// WASMの定跡データをOpeningMoveに変換
export const convertWasmMoveToOpeningMove = (
    wasmMove: WasmMoveData,
    board: Board,
    logger = createLogger("Convert", false),
): OpeningMove | null => {
    const parsed = parseNotation(wasmMove.notation);
    if (!parsed) {
        logger.warn(`Failed to parse notation: ${wasmMove.notation}`);
        return null;
    }

    const fromKey = squareToKey(parsed.fromRow, parsed.fromCol);
    const piece = board[fromKey];

    if (!piece) {
        logger.warn(`No piece found at ${fromKey}`);
        return null;
    }

    const toKey = squareToKey(parsed.toRow, parsed.toCol);
    const captured = board[toKey] || null;

    const move: Move = {
        type: "move",
        from: { row: parsed.fromRow, column: parsed.fromCol },
        to: { row: parsed.toRow, column: parsed.toCol },
        piece,
        promote: parsed.promote,
        captured,
    };

    return {
        move,
        notation: wasmMove.notation,
        weight: wasmMove.evaluation || 1,
        depth: wasmMove.depth,
    };
};

// 位置情報をSFEN形式に変換
export const positionToSfen = (position: PositionState, moveCount: number): string => {
    return exportToSfen(position.board, position.hands, position.currentPlayer, moveCount);
};

// ===========================================
// OpeningBook実装
// ===========================================

// WASMベースの定跡実装を作成
const createWasmOpeningBook = (
    reader: OpeningBookReaderWasm,
    logger = createLogger("OpeningBook", false),
): OpeningBookInterface => {
    // SFEN形式で定跡を検索
    const searchBySfen = (sfen: string): WasmMoveData[] => {
        const sfenForSearch = sfen.startsWith("sfen ") ? sfen.slice(5) : sfen;

        try {
            const movesJson = reader.find_moves(sfenForSearch);
            const moves = JSON.parse(movesJson) as WasmMoveData[];
            return moves || [];
        } catch (error) {
            logger.error("Failed to search moves", error);
            return [];
        }
    };

    // FindMoves実装
    const findMoves = (position: PositionState, options?: FindMovesOptions): OpeningMove[] => {
        const moveCount = options?.moveHistory ? options.moveHistory.length + 1 : 1;

        let sfen: string;
        try {
            sfen = positionToSfen(position, moveCount);
        } catch (error) {
            logger.error("Failed to convert position to SFEN", error);
            return [];
        }

        const wasmMoves = searchBySfen(sfen);

        if (wasmMoves.length === 0) {
            logger.debug(`No moves found for position: ${sfen}`);
            return [];
        }

        const openingMoves: OpeningMove[] = [];

        for (const wasmMove of wasmMoves) {
            const openingMove = convertWasmMoveToOpeningMove(wasmMove, position.board, logger);

            if (openingMove) {
                openingMoves.push(openingMove);
            }
        }

        // 重み付きランダム選択（オプション）
        if (options?.randomize && openingMoves.length > 0) {
            return [selectWeightedRandom(openingMoves)];
        }

        return openingMoves;
    };

    return {
        addEntry: () => {
            logger.warn("addEntry is not supported in WASM implementation");
            return false;
        },
        findMoves,
        size: () => reader.position_count,
    };
};

// 重み付きランダム選択
export const selectWeightedRandom = (moves: OpeningMove[]): OpeningMove => {
    const totalWeight = moves.reduce((sum, move) => sum + move.weight, 0);
    let random = Math.random() * totalWeight;

    for (const move of moves) {
        random -= move.weight;
        if (random <= 0) {
            return move;
        }
    }

    return moves[0]; // フォールバック
};

// ===========================================
// メインファクトリー
// ===========================================

/**
 * WASM定跡ローダーのファクトリー関数
 * インスタンスごとに独立した状態を保持
 */
export function createWasmOpeningBookLoader(options?: {
    logger?: ReturnType<typeof createLogger>;
    wasmManager?: WasmManager;
}): OpeningBookLoaderInterface {
    // インスタンスごとの状態
    const logger = options?.logger ?? createLogger("WasmOpeningBookLoader");
    const wasmManager = options?.wasmManager ?? getDefaultWasmManager();
    let reader: OpeningBookReaderWasm | null = null;
    const loadedFiles = new Set<string>();

    // リーダーの初期化
    const ensureReader = async (): Promise<OpeningBookReaderWasm> => {
        if (reader) return reader;

        await wasmManager.initialize();
        reader = new OpeningBookReaderWasm();
        logger.info("Reader created successfully");

        return reader;
    };

    // ファイル読み込み
    const loadFile = async (filePath: string): Promise<void> => {
        if (loadedFiles.has(filePath)) {
            logger.info(`File already loaded: ${filePath}`);
            return;
        }

        const currentReader = await ensureReader();
        const data = await fetchOpeningBookData(filePath, logger);

        const result = currentReader.load_data(data);
        logger.info(`Load result: ${result}`);

        loadedFiles.add(filePath);
        logger.info(`Successfully loaded ${filePath}, positions: ${currentReader.position_count}`);
    };

    // OpeningBookLoaderInterface実装
    const load = async (filePath: string): Promise<OpeningBookInterface> => {
        logger.info(`Loading opening book from: ${filePath}`);

        try {
            await loadFile(filePath);
            const currentReader = await ensureReader();
            return createWasmOpeningBook(currentReader, logger);
        } catch (error) {
            const message = error instanceof Error ? error.message : "Unknown error";
            throw new Error(`Failed to load opening book: ${message}`);
        }
    };

    const loadForDifficulty = async (difficulty: AIDifficulty): Promise<OpeningBookInterface> => {
        const filePath = DIFFICULTY_FILES[difficulty];
        return load(filePath);
    };

    return { load, loadForDifficulty };
}
