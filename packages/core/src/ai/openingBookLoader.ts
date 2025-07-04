import * as pako from "pako";
import { OpeningBook, type OpeningEntry } from "./openingBook";

/**
 * 大容量定跡データベースローダー
 * 分割された圧縮ファイルを効率的に読み込み
 */
export class OpeningBookLoader {
    private indexUrl: string;
    private baseUrl: string;
    protected loadedFiles: Set<string> = new Set();
    protected openingBook: OpeningBook;
    protected isLoading = false;
    protected loadProgress = 0;
    protected isInitialLoad = true;
    protected maxDepthLimit = 10;

    constructor(baseUrl: string) {
        this.baseUrl = baseUrl;
        this.indexUrl = `${baseUrl}/index.json`;
        this.openingBook = new OpeningBook(100); // 最大100手まで
    }

    /**
     * 定跡データベースを初期化
     * @param options ロードオプション
     */
    async initialize(options?: {
        onProgress?: (progress: number) => void;
        preloadCount?: number; // 初期読み込みファイル数
        maxInitialDepth?: number; // 初期読み込みの最大深さ
    }): Promise<void> {
        const { onProgress, preloadCount = 5, maxInitialDepth = 10 } = options || {};
        this.maxDepthLimit = maxInitialDepth;

        try {
            // インデックスファイルを読み込み
            const indexResponse = await fetch(this.indexUrl);
            if (!indexResponse.ok) {
                throw new Error(`Failed to load index: ${indexResponse.status}`);
            }

            const index = await indexResponse.json();
            console.log(`📚 定跡データベース: ${index.totalEntries.toLocaleString()} エントリ`);

            // 初期ファイルを読み込み
            const files = index.files.slice(0, preloadCount);
            await this.loadFiles(files, onProgress);

            console.log(
                `✅ 初期読み込み完了: ${this.openingBook.size()} エントリ (${this.openingBook.getMemoryUsageMB()} MB)`,
            );

            // 初期ロードフラグを解除
            this.isInitialLoad = false;

            // 残りのファイルをバックグラウンドで読み込み
            if (index.files.length > preloadCount) {
                this.loadRemainingFilesInBackground(index.files.slice(preloadCount), onProgress);
            }
        } catch (error) {
            console.error("❌ 定跡データベース初期化エラー:", error);
            throw error;
        }
    }

    /**
     * ファイルを読み込み
     */
    private async loadFiles(
        files: Array<{ name: string; size: number }>,
        onProgress?: (progress: number) => void,
    ): Promise<void> {
        for (let i = 0; i < files.length; i++) {
            const file = files[i];
            if (this.loadedFiles.has(file.name)) {
                continue;
            }

            await this.loadSingleFile(file.name);
            this.loadedFiles.add(file.name);

            // 進捗更新
            this.loadProgress = ((i + 1) / files.length) * 100;
            onProgress?.(this.loadProgress);
        }
    }

    /**
     * 単一ファイルを読み込み
     */
    private async loadSingleFile(fileName: string): Promise<void> {
        // .gz拡張子を.gzipに変更（Viteの自動解凍を回避）
        const adjustedFileName = fileName.replace(".json.gz", ".gzip");
        const url = `${this.baseUrl}/${adjustedFileName}`;

        try {
            // ファイルを取得
            const response = await fetch(url);
            if (!response.ok) {
                // フォールバック: 元のファイル名で試す
                const fallbackUrl = `${this.baseUrl}/${fileName}`;
                const fallbackResponse = await fetch(fallbackUrl);
                if (!fallbackResponse.ok) {
                    throw new Error(`Failed to load ${fileName}: ${response.status}`);
                }
                return this.processResponse(fallbackResponse, fileName);
            }

            return this.processResponse(response, fileName);
        } catch (error) {
            const errorMessage = error instanceof Error ? error.message : String(error);
            console.error(`❌ ${fileName} 読み込みエラー:`, errorMessage);
            throw error;
        }
    }

    protected async processResponse(response: Response, fileName: string): Promise<void> {
        try {
            // データの取得と解析
            const arrayBuffer = await response.arrayBuffer();
            const uint8Array = new Uint8Array(arrayBuffer);

            // 最初の数バイトでデータ形式を判定
            let data: { entries: OpeningEntry[] };
            if (uint8Array.length >= 2 && uint8Array[0] === 0x1f && uint8Array[1] === 0x8b) {
                // gzip圧縮されている（マジックナンバー: 0x1f8b）
                const decompressed = pako.ungzip(uint8Array);
                const jsonString = new TextDecoder().decode(decompressed);
                data = JSON.parse(jsonString);
            } else if (
                uint8Array.length > 0 &&
                (uint8Array[0] === 0x5b || uint8Array[0] === 0x7b)
            ) {
                // JSONデータ（'[' = 0x5b または '{' = 0x7b で始まる）
                const jsonString = new TextDecoder().decode(uint8Array);
                data = JSON.parse(jsonString);
            } else {
                throw new Error(`Unknown data format for ${fileName}`);
            }

            // エントリを定跡データベースに増分追加（メモリ効率化）
            // 初期ロード時は深さ制限を適用
            if (this.isInitialLoad && this.maxDepthLimit > 0) {
                const filteredEntries = data.entries.filter(
                    (entry: OpeningEntry) => entry.depth <= this.maxDepthLimit,
                );
                this.openingBook.addEntries(filteredEntries);
                console.log(
                    `📖 ${fileName}: ${filteredEntries.length}/${data.entries.length} エントリ追加（深さ${this.maxDepthLimit}まで）`,
                );
            } else {
                this.openingBook.addEntries(data.entries);
                console.log(`📖 ${fileName}: ${data.entries.length} エントリ追加`);
            }
        } catch (error) {
            console.error(`❌ ${fileName} 読み込みエラー:`, error);
        }
    }

    /**
     * 残りのファイルをバックグラウンドで読み込み
     */
    private async loadRemainingFilesInBackground(
        files: Array<{ name: string; size: number }>,
        onProgress?: (progress: number) => void,
    ): Promise<void> {
        if (this.isLoading) {
            return;
        }

        this.isLoading = true;

        // Web Workerが使える場合は別スレッドで実行
        if (typeof Worker !== "undefined") {
            // TODO: Web Worker実装
            await this.loadFilesSequentially(files, onProgress);
        } else {
            // メインスレッドで順次読み込み
            await this.loadFilesSequentially(files, onProgress);
        }

        this.isLoading = false;
        console.log(
            `✅ 全定跡読み込み完了: ${this.openingBook.size()} エントリ (${this.openingBook.getMemoryUsageMB()} MB)`,
        );
    }

    /**
     * ファイルを順次読み込み
     */
    private async loadFilesSequentially(
        files: Array<{ name: string; size: number }>,
        onProgress?: (progress: number) => void,
    ): Promise<void> {
        for (let i = 0; i < files.length; i++) {
            // 短い遅延を入れてUIをブロックしない
            await new Promise((resolve) => setTimeout(resolve, 10));

            await this.loadSingleFile(files[i].name);
            this.loadedFiles.add(files[i].name);

            // 全体の進捗を計算
            const totalProgress = (this.loadedFiles.size / (files.length + 5)) * 100;
            onProgress?.(totalProgress);

            // 定期的にメモリチェック（10ファイルごと）
            if (i % 10 === 0) {
                await this.checkMemoryAndReduce();
            }
        }
    }

    /**
     * 定跡を取得
     */
    getOpeningBook(): OpeningBook {
        return this.openingBook;
    }

    /**
     * 読み込み進捗を取得
     */
    getProgress(): number {
        return this.loadProgress;
    }

    /**
     * 読み込み中かどうか
     */
    isLoadingData(): boolean {
        return this.isLoading;
    }

    /**
     * メモリ使用量の推定
     */
    estimateMemoryUsage(): number {
        return this.openingBook.estimateMemoryUsage();
    }

    /**
     * メモリ使用量の確認と自動削減
     */
    private async checkMemoryAndReduce(): Promise<void> {
        const memoryUsageMB = this.openingBook.getMemoryUsageMB();
        const MAX_MEMORY_MB = 200; // 最大200MBまで

        if (memoryUsageMB > MAX_MEMORY_MB) {
            console.warn(
                `⚠️ メモリ使用量が制限を超えています: ${memoryUsageMB}MB > ${MAX_MEMORY_MB}MB`,
            );
            console.log("📉 深い定跡を削除してメモリを節約します...");

            // 深さ20以上の定跡を削除
            this.openingBook.removeDeepEntries(20);

            const newMemoryUsageMB = this.openingBook.getMemoryUsageMB();
            console.log(`✅ メモリ削減完了: ${memoryUsageMB}MB → ${newMemoryUsageMB}MB`);
        }
    }
}

/**
 * シングルトンインスタンス管理
 */
let globalLoader: OpeningBookLoader | null = null;

export async function getOpeningBookLoader(baseUrl: string): Promise<OpeningBookLoader> {
    if (!globalLoader) {
        globalLoader = new OpeningBookLoader(baseUrl);
        await globalLoader.initialize();
    }
    return globalLoader;
}
