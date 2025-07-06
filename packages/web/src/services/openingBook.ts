import type { BookLevel, BookMove, LoadProgress } from "@/types/openingBook";
import { OpeningBookReaderWasm } from "@/wasm/shogi_core";

export type ProgressCallback = (progress: LoadProgress) => void;

export class OpeningBookService {
    private initialized = false;
    private reader: OpeningBookReaderWasm | null = null;
    private currentLevel: BookLevel | null = null;

    async initialize(): Promise<void> {
        if (this.initialized) return;

        try {
            // WASMモジュールを初期化
            this.reader = new OpeningBookReaderWasm();
            this.initialized = true;
        } catch (error) {
            this.initialized = false;
            throw error;
        }
    }

    isInitialized(): boolean {
        return this.initialized;
    }

    // テスト用のgetter（本番では使用しない）
    getReader(): OpeningBookReaderWasm | null {
        return this.reader;
    }

    async loadBook(level: BookLevel = "early", onProgress?: ProgressCallback): Promise<void> {
        if (!this.initialized) {
            await this.initialize();
        }

        if (this.currentLevel === level) return;

        const url = `/data/opening_book_${level}.bin.binz`;
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

        if (this.reader) {
            const result = this.reader.load_data(data);
            console.log(`OpeningBook loaded: ${result}`);
            onProgress?.({ phase: "decompressing", loaded: 100, total: 100 });
        }

        this.currentLevel = level;
    }

    async findMoves(sfen: string): Promise<BookMove[]> {
        if (!this.initialized || !this.reader) return [];

        try {
            // WASMから手を取得
            const movesJson = this.reader.find_moves(sfen);
            return JSON.parse(movesJson);
        } catch (error) {
            console.error("Failed to find moves:", error);
            return [];
        }
    }
}

export const openingBook = new OpeningBookService();
