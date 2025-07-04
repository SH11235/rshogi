#!/usr/bin/env node
import * as fs from "node:fs";
import * as path from "node:path";
import * as readline from "node:readline";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import { createGzip } from "node:zlib";
import type { OpeningEntry } from "../src/ai/openingBook";
import { convertMove, getMoveString, parseLine, valueToWinRate } from "./yaneuraouParser";

/**
 * YaneuraOu定跡データベースを変換するスクリプト
 * 500MBの定跡ファイルを効率的に処理し、Web用に最適化
 */

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// 入力ファイルパス（必要に応じて変更）
const INPUT_FILE = process.argv[2] || path.join(__dirname, "../../../user_book1.db");
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

interface ConversionState {
    stats: ConversionStats;
    currentFileIndex: number;
    currentFileSize: number;
    openingEntries: OpeningEntry[];
    positionMap: Map<string, OpeningEntry>;
}

/**
 * 状態を初期化
 */
function createInitialState(): ConversionState {
    return {
        stats: {
            totalLines: 0,
            totalEntries: 0,
            skippedEntries: 0,
            outputFiles: [],
            startTime: Date.now(),
            memoryUsage: process.memoryUsage(),
        },
        currentFileIndex: 0,
        currentFileSize: 0,
        openingEntries: [],
        positionMap: new Map(),
    };
}

/**
 * 出力ディレクトリを準備
 */
function prepareOutputDir(): void {
    if (fs.existsSync(OUTPUT_DIR)) {
        fs.rmSync(OUTPUT_DIR, { recursive: true });
    }
    fs.mkdirSync(OUTPUT_DIR, { recursive: true });
}

/**
 * 行を処理して定跡エントリに変換
 */
function processLine(line: string, state: ConversionState, lineNumber: number): void {
    const parsed = parseLine(line);
    if (!parsed) {
        state.stats.skippedEntries++;
        return;
    }

    const move = convertMove(parsed.bestMove);
    if (!move) {
        state.stats.skippedEntries++;
        return;
    }

    // 局面が既に存在するかチェック
    let entry = state.positionMap.get(parsed.sfen);
    if (!entry) {
        entry = {
            position: parsed.sfen,
            depth: lineNumber, // 仮の深さ（後で計算可能）
            moves: [],
        };
        state.positionMap.set(parsed.sfen, entry);
        state.openingEntries.push(entry);
    }

    // 手を追加（重複チェック）
    const moveExists = entry.moves.some(
        (m) =>
            m.move.type === move.type &&
            ((move.type === "move" &&
                m.move.type === "move" &&
                m.move.from.row === move.from.row &&
                m.move.from.column === move.from.column &&
                m.move.to.row === move.to.row &&
                m.move.to.column === move.to.column) ||
                (move.type === "drop" &&
                    m.move.type === "drop" &&
                    m.move.piece.type === move.piece.type &&
                    m.move.to.row === move.to.row &&
                    m.move.to.column === move.to.column)),
    );

    if (!moveExists) {
        const moveString = getMoveString(parsed.bestMove);
        entry.moves.push({
            move,
            weight: Math.max(1, Math.round(valueToWinRate(parsed.value) * 100)),
            comment: moveString,
        });
    }

    state.stats.totalEntries++;
}

/**
 * 現在のバッチをファイルに書き込み
 */
async function writeCurrentBatch(state: ConversionState): Promise<void> {
    if (state.openingEntries.length === 0) return;

    const fileName = `openings-${String(state.currentFileIndex).padStart(3, "0")}.json`;
    const filePath = path.join(OUTPUT_DIR, fileName);
    const gzipPath = `${filePath}.gz`;

    // JSONファイルを一時的に作成
    const data = {
        version: "1.0.0",
        entries: state.openingEntries,
    };
    fs.writeFileSync(filePath, JSON.stringify(data));

    // gzip圧縮
    const source = fs.createReadStream(filePath);
    const destination = fs.createWriteStream(gzipPath);
    const gzip = createGzip({ level: 9 });

    await pipeline(source, gzip, destination);

    // 一時ファイルを削除
    fs.unlinkSync(filePath);

    state.stats.outputFiles.push(gzipPath);
    state.currentFileIndex++;
    state.currentFileSize = 0;
    state.openingEntries = [];
}

/**
 * インデックスファイルを作成
 */
function createIndexFile(state: ConversionState): void {
    const indexPath = path.join(OUTPUT_DIR, "index.json");
    const index = {
        version: "1.0.0",
        createdAt: new Date().toISOString(),
        totalEntries: state.stats.totalEntries,
        files: state.stats.outputFiles.map((file) => ({
            name: path.basename(file),
            size: fs.statSync(file).size,
        })),
    };

    fs.writeFileSync(indexPath, JSON.stringify(index, null, 2));
    console.log(`\n📑 インデックスファイル作成: ${indexPath}`);
}

/**
 * 進捗を表示
 */
function printProgress(lineNumber: number, state: ConversionState): void {
    const elapsed = (Date.now() - state.stats.startTime) / 1000;
    const linesPerSecond = Math.round(lineNumber / elapsed);
    const memoryMB = Math.round(state.stats.memoryUsage.heapUsed / 1024 / 1024);

    process.stdout.write(
        `\r⏳ 処理中: ${lineNumber.toLocaleString()} 行 | ` +
            `${state.stats.totalEntries.toLocaleString()} エントリ | ` +
            `${linesPerSecond.toLocaleString()} 行/秒 | ` +
            `メモリ: ${memoryMB} MB`,
    );
}

/**
 * 統計情報を表示
 */
function printStats(state: ConversionState): void {
    const elapsed = (Date.now() - state.stats.startTime) / 1000;
    const totalSizeMB =
        state.stats.outputFiles.reduce((sum, file) => {
            return sum + fs.statSync(file).size;
        }, 0) /
        1024 /
        1024;

    console.log("\n\n✅ 変換完了！");
    console.log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    console.log("📊 処理統計:");
    console.log(`  - 総行数: ${state.stats.totalLines.toLocaleString()}`);
    console.log(`  - 総エントリ数: ${state.stats.totalEntries.toLocaleString()}`);
    console.log(`  - スキップ数: ${state.stats.skippedEntries.toLocaleString()}`);
    console.log(`  - 出力ファイル数: ${state.stats.outputFiles.length}`);
    console.log(`  - 総出力サイズ: ${totalSizeMB.toFixed(2)} MB (圧縮後)`);
    console.log(`  - 処理時間: ${elapsed.toFixed(1)} 秒`);
    console.log(
        `  - 平均速度: ${Math.round(state.stats.totalLines / elapsed).toLocaleString()} 行/秒`,
    );
    console.log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
}

/**
 * メイン変換処理
 */
async function convert(): Promise<void> {
    const state = createInitialState();

    console.log("🚀 YaneuraOu定跡データベース変換開始");
    console.log(`📁 入力: ${INPUT_FILE}`);
    console.log(`📂 出力: ${OUTPUT_DIR}/`);
    console.log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    prepareOutputDir();

    const fileStream = fs.createReadStream(INPUT_FILE);
    const rl = readline.createInterface({
        input: fileStream,
        crlfDelay: Number.POSITIVE_INFINITY,
    });

    let lineNumber = 0;

    for await (const line of rl) {
        lineNumber++;
        state.stats.totalLines++;

        // 空行やコメント行をスキップ
        if (!line.trim() || line.startsWith("#")) {
            continue;
        }

        processLine(line, state, lineNumber);

        // 定期的にファイルに書き込み
        const estimatedSize = JSON.stringify(state.openingEntries).length;
        if (estimatedSize > OUTPUT_FILE_SIZE || state.openingEntries.length > BATCH_SIZE) {
            await writeCurrentBatch(state);
            state.positionMap.clear(); // メモリ解放
        }

        // 進捗表示
        if (lineNumber % 10000 === 0) {
            state.stats.memoryUsage = process.memoryUsage();
            printProgress(lineNumber, state);
        }
    }

    // 残りのデータを書き込み
    await writeCurrentBatch(state);

    // インデックスファイル作成
    createIndexFile(state);

    // 統計情報表示
    printStats(state);
}

// メイン処理
async function main() {
    try {
        // ファイル存在確認
        if (!fs.existsSync(INPUT_FILE)) {
            console.error(`❌ エラー: 入力ファイルが見つかりません: ${INPUT_FILE}`);
            console.log("\n使用方法:");
            console.log("  npm run tsx scripts/convertOpeningBook.ts [入力ファイルパス]");
            console.log("\n例:");
            console.log("  npm run tsx scripts/convertOpeningBook.ts ../../../user_book1.db");
            process.exit(1);
        }

        await convert();
    } catch (error) {
        console.error(`\n❌ 致命的エラー: ${error}`);
        process.exit(1);
    }
}

// 実行
if (import.meta.url === `file://${process.argv[1]}`) {
    main().catch(console.error);
}
