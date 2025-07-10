import type { BookLevel, BookMove, LoadProgress } from "@/types/openingBook";
import init, { OpeningBookReaderWasm } from "@/wasm/shogi_core";

export type ProgressCallback = (progress: LoadProgress) => void;

// モジュールレベルの状態管理
const state = {
    initialized: false,
    reader: null as OpeningBookReaderWasm | null,
    currentLevel: null as BookLevel | null,
};

async function initialize(): Promise<void> {
    if (state.initialized) return;

    try {
        // WASMモジュールを初期化
        await init();

        // WASMが初期化された後でインスタンスを作成
        state.reader = new OpeningBookReaderWasm();
        state.initialized = true;
    } catch (error) {
        state.initialized = false;
        throw error;
    }
}

function isInitialized(): boolean {
    return state.initialized;
}

// テスト用のgetter（本番では使用しない）
function getReader(): OpeningBookReaderWasm | null {
    return state.reader;
}

async function loadBook(level: BookLevel = "early", onProgress?: ProgressCallback): Promise<void> {
    if (!state.initialized) {
        await initialize();
    }

    if (state.currentLevel === level) return;

    const url = `./data/opening_book_${level}.binz`;
    const response = await fetch(url);
    const contentLength = Number(response.headers.get("content-length") || 0);

    if (!response.body) {
        throw new Error("Response body is empty");
    }
    const reader = response.body.getReader();
    let receivedLength = 0;
    const chunks: Uint8Array[] = [];

    while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        chunks.push(value);
        receivedLength += value.length;

        onProgress?.({
            phase: "downloading",
            loaded: receivedLength,
            total: contentLength,
        });
    }

    // データを結合
    const data = new Uint8Array(receivedLength);
    let position = 0;
    for (const chunk of chunks) {
        data.set(chunk, position);
        position += chunk.length;
    }

    // WASMに読み込む
    onProgress?.({ phase: "decompressing", loaded: 0, total: 100 });

    if (state.reader) {
        const result = state.reader.load_data(data);
        console.log(`OpeningBook loaded: ${result}`);
        onProgress?.({ phase: "decompressing", loaded: 100, total: 100 });
    }

    state.currentLevel = level;
}

async function findMoves(sfen: string): Promise<BookMove[]> {
    if (!state.initialized || !state.reader) return [];

    try {
        // WASMから手を取得
        console.log(`Searching moves for SFEN: ${sfen}`);
        const movesJson = state.reader.find_moves(sfen);
        return JSON.parse(movesJson);
    } catch (error) {
        console.error("Failed to find moves:", error);
        return [];
    }
}

// 公開API
export const openingBook = {
    initialize,
    isInitialized,
    getReader,
    loadBook,
    findMoves,
};
