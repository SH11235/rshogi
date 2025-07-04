#!/usr/bin/env node
import * as fs from "node:fs";
import * as path from "node:path";
import * as readline from "node:readline";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";
import { createGzip } from "node:zlib";
import type { OpeningEntry } from "../src/ai/openingBook";
import { YaneuraOuParser } from "../src/ai/yaneuraouParser";

/**
 * YaneuraOu定跡データベースを変換するスクリプト
 * 500MBの定跡ファイルを効率的に処理し、Web用に最適化
 */

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const INPUT_FILE = path.join(__dirname, "../../../user_book1.db");
const OUTPUT_DIR = path.join(__dirname, "../data/openings");
const BATCH_SIZE = 100000; // 10万行ごとに処理
const OUTPUT_FILE_SIZE = 50 * 1024 * 1024; // 50MB

interface ConversionStats {
    totalLines: number;
    totalEntries: number;
    skippedEntries: number;
    outputFiles: string[];
    startTime: number;
    memoryUsage: NodeJS.MemoryUsage;
}

class OpeningBookConverter {
    private parser: YaneuraOuParser;
    private stats: ConversionStats;
    private currentBatch: string[];
    private currentFileIndex: number;
    private currentFileSize: number;
    private openingEntries: OpeningEntry[];

    constructor() {
        this.parser = new YaneuraOuParser();
        this.stats = {
            totalLines: 0,
            totalEntries: 0,
            skippedEntries: 0,
            outputFiles: [],
            startTime: Date.now(),
            memoryUsage: process.memoryUsage(),
        };
        this.currentBatch = [];
        this.currentFileIndex = 0;
        this.currentFileSize = 0;
        this.openingEntries = [];
    }

    async convert(): Promise<void> {
        console.log("🔄 YaneuraOu定跡データベース変換開始...");
        console.log(`📁 入力ファイル: ${INPUT_FILE}`);

        // 出力ディレクトリ作成
        if (!fs.existsSync(OUTPUT_DIR)) {
            fs.mkdirSync(OUTPUT_DIR, { recursive: true });
        }

        // ファイルサイズ確認
        const fileStats = fs.statSync(INPUT_FILE);
        console.log(`📊 ファイルサイズ: ${(fileStats.size / 1024 / 1024).toFixed(2)} MB`);

        // ストリーミング読み込み
        await this.processFile();

        // 最後のバッチを処理
        if (this.currentBatch.length > 0) {
            await this.processBatch();
        }

        // 最後のファイルを保存
        if (this.openingEntries.length > 0) {
            await this.saveCurrentFile();
        }

        // インデックスファイルを作成
        await this.createIndexFile();

        // 統計情報を表示
        this.printStats();
    }

    private async processFile(): Promise<void> {
        const fileStream = fs.createReadStream(INPUT_FILE);
        const rl = readline.createInterface({
            input: fileStream,
            crlfDelay: Number.POSITIVE_INFINITY,
        });

        let lineNumber = 0;

        for await (const line of rl) {
            lineNumber++;
            this.stats.totalLines++;

            // 進捗表示
            if (lineNumber % 10000 === 0) {
                this.printProgress(lineNumber);
            }

            this.currentBatch.push(line);

            // バッチサイズに達したら処理
            if (this.currentBatch.length >= BATCH_SIZE) {
                await this.processBatch();
            }
        }
    }

    private async processBatch(): Promise<void> {
        const batchContent = this.currentBatch.join("\n");
        this.currentBatch = [];

        try {
            // バッチをパース
            const entries = this.parser.parseDatabase(batchContent);

            for (const entry of entries) {
                try {
                    // 内部形式に変換
                    const openingEntry = this.parser.convertToOpeningEntry(entry);
                    this.openingEntries.push(openingEntry);
                    this.stats.totalEntries++;

                    // 推定サイズを計算
                    const entrySize = JSON.stringify(openingEntry).length;
                    this.currentFileSize += entrySize;
                } catch (error) {
                    // 変換エラーはスキップ
                    this.stats.skippedEntries++;
                }
            }

            // バッチ処理後にファイルサイズをチェック
            if (this.currentFileSize >= OUTPUT_FILE_SIZE && this.openingEntries.length > 0) {
                await this.saveCurrentFile();
            }
        } catch (error) {
            console.error(`⚠️ バッチ処理エラー: ${error}`);
        }

        // メモリ使用量を更新
        this.stats.memoryUsage = process.memoryUsage();

        // ガベージコレクションのヒント
        if (global.gc) {
            global.gc();
        }
    }

    private async saveCurrentFile(): Promise<void> {
        const fileName = `openings-${String(this.currentFileIndex).padStart(3, "0")}.json`;
        const filePath = path.join(OUTPUT_DIR, fileName);
        const gzipPath = `${filePath}.gz`;

        console.log(`\n💾 ファイル保存中: ${fileName} (${this.openingEntries.length} エントリ)`);

        // JSON形式で保存
        const jsonContent = JSON.stringify({
            version: "1.0.0",
            source: "YaneuraOu user_book1.db",
            entries: this.openingEntries,
        });

        // 一時ファイルに書き込んでからgzip圧縮
        fs.writeFileSync(filePath, jsonContent);

        // gzip圧縮して保存
        const source = fs.createReadStream(filePath);
        const destination = fs.createWriteStream(gzipPath);
        const gzip = createGzip({ level: 9 });

        await pipeline(source, gzip, destination);

        // 一時ファイルを削除
        fs.unlinkSync(filePath);

        this.stats.outputFiles.push(gzipPath);
        this.currentFileIndex++;
        this.currentFileSize = 0;
        this.openingEntries = [];
    }

    private async createIndexFile(): Promise<void> {
        const indexPath = path.join(OUTPUT_DIR, "index.json");
        const index = {
            version: "1.0.0",
            createdAt: new Date().toISOString(),
            totalEntries: this.stats.totalEntries,
            files: this.stats.outputFiles.map((file) => ({
                name: path.basename(file),
                size: fs.statSync(file).size,
            })),
        };

        fs.writeFileSync(indexPath, JSON.stringify(index, null, 2));
        console.log(`\n📑 インデックスファイル作成: ${indexPath}`);
    }

    private printProgress(lineNumber: number): void {
        const elapsed = (Date.now() - this.stats.startTime) / 1000;
        const linesPerSecond = Math.round(lineNumber / elapsed);
        const memoryMB = Math.round(this.stats.memoryUsage.heapUsed / 1024 / 1024);

        process.stdout.write(
            `\r⏳ 処理中: ${lineNumber.toLocaleString()} 行 | ` +
                `${this.stats.totalEntries.toLocaleString()} エントリ | ` +
                `${linesPerSecond.toLocaleString()} 行/秒 | ` +
                `メモリ: ${memoryMB} MB`,
        );
    }

    private printStats(): void {
        const elapsed = (Date.now() - this.stats.startTime) / 1000;
        const totalSizeMB =
            this.stats.outputFiles.reduce((sum, file) => {
                return sum + fs.statSync(file).size;
            }, 0) /
            1024 /
            1024;

        console.log("\n\n✅ 変換完了！");
        console.log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        console.log(`📊 処理統計:`);
        console.log(`  - 総行数: ${this.stats.totalLines.toLocaleString()}`);
        console.log(`  - 総エントリ数: ${this.stats.totalEntries.toLocaleString()}`);
        console.log(`  - スキップ数: ${this.stats.skippedEntries.toLocaleString()}`);
        console.log(`  - 出力ファイル数: ${this.stats.outputFiles.length}`);
        console.log(`  - 総出力サイズ: ${totalSizeMB.toFixed(2)} MB (圧縮後)`);
        console.log(`  - 処理時間: ${elapsed.toFixed(1)} 秒`);
        console.log(
            `  - 平均速度: ${Math.round(this.stats.totalLines / elapsed).toLocaleString()} 行/秒`,
        );
        console.log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    }
}

// メイン処理
async function main() {
    try {
        // ファイル存在確認
        if (!fs.existsSync(INPUT_FILE)) {
            console.error(`❌ エラー: 入力ファイルが見つかりません: ${INPUT_FILE}`);
            process.exit(1);
        }

        const converter = new OpeningBookConverter();
        await converter.convert();
    } catch (error) {
        console.error(`\n❌ 致命的エラー: ${error}`);
        process.exit(1);
    }
}

// Node.jsで直接実行された場合
if (import.meta.url === `file://${process.argv[1]}`) {
    main();
}

export { OpeningBookConverter };
