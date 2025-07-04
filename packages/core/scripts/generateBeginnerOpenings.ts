#!/usr/bin/env node
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";
import type { OpeningEntry } from "../src/ai/openingBook";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

/**
 * gzipファイルから定跡データを読み込む
 */
function loadOpeningFile(filePath: string): OpeningEntry[] {
    try {
        const compressed = fs.readFileSync(filePath);
        const decompressed = gunzipSync(compressed);
        const data = JSON.parse(decompressed.toString());
        return data.entries || [];
    } catch (error) {
        console.error(`ファイル読み込みエラー (${filePath}):`, error);
        return [];
    }
}

/**
 * ビギナー向け定跡データを生成
 */
async function generateBeginnerOpenings() {
    console.log("🎯 ビギナー向け定跡データ生成開始...");

    try {
        // 定跡データのパス
        const openingsPath = path.join(__dirname, "../data/openings");

        // 最初の2ファイルから定跡エントリーを読み込み
        const allEntries: OpeningEntry[] = [];

        for (let i = 0; i < 2; i++) {
            const fileName = `openings-${String(i).padStart(3, "0")}.gzip`;
            const filePath = path.join(openingsPath, fileName);

            if (fs.existsSync(filePath)) {
                console.log(`📖 読み込み中: ${fileName}`);
                const entries = loadOpeningFile(filePath);
                allEntries.push(...entries);
                console.log(`  → ${entries.length} エントリ読み込み完了`);
            }
        }

        console.log(`\n📊 総エントリ数: ${allEntries.length}`);

        // ビギナー向けにフィルタリング
        const beginnerEntries = allEntries.filter((entry) => {
            // 条件：
            // 1. 序盤（10手目まで）に限定
            // 2. 各手のweightが50以上のものが含まれる（主要な手のみ）
            return entry.depth <= 10 && entry.moves.some((m) => m.weight && m.weight >= 50);
        });

        console.log(`🔍 フィルタ後: ${beginnerEntries.length} エントリ`);

        // 各局面で人気のある手を選択
        const processedEntries = beginnerEntries.map((entry) => {
            // weight順にソートして上位手を選択
            const sortedMoves = entry.moves
                .filter((m) => m.weight && m.weight >= 50)
                .sort((a, b) => (b.weight || 0) - (a.weight || 0));

            // 局面の深さに応じて選択する手数を調整
            let maxMoves = 3; // デフォルト
            if (entry.depth <= 5) {
                maxMoves = 4; // 序盤は選択肢を少し多めに
            } else if (entry.depth > 10) {
                maxMoves = 2; // 中盤以降は絞る
            }

            return {
                ...entry,
                moves: sortedMoves.slice(0, maxMoves),
            };
        });

        // 統計情報を計算
        const stats = {
            totalEntries: processedEntries.length,
            byDepth: {} as Record<number, number>,
            averageMovesPerPosition: 0,
        };

        let totalMoves = 0;
        for (const entry of processedEntries) {
            stats.byDepth[entry.depth] = (stats.byDepth[entry.depth] || 0) + 1;
            totalMoves += entry.moves.length;
        }
        stats.averageMovesPerPosition = totalMoves / processedEntries.length;

        console.log("\n📈 統計情報:");
        console.log(`  - 総エントリ数: ${stats.totalEntries}`);
        console.log(`  - 局面あたり平均手数: ${stats.averageMovesPerPosition.toFixed(2)}`);
        console.log("  - 手数別分布:");

        for (let depth = 0; depth <= 20; depth++) {
            if (stats.byDepth[depth]) {
                console.log(`    ${depth}手目: ${stats.byDepth[depth]} 局面`);
            }
        }

        // 出力データを作成
        const outputData = {
            version: "1.0.0",
            createdAt: new Date().toISOString(),
            source: "YaneuraOu user_book1.db (filtered for beginners)",
            stats,
            entries: processedEntries,
        };

        // ファイルに保存
        const outputPath = path.join(__dirname, "../../web/public/data/beginner-openings.json");
        const outputDir = path.dirname(outputPath);

        if (!fs.existsSync(outputDir)) {
            fs.mkdirSync(outputDir, { recursive: true });
        }

        fs.writeFileSync(outputPath, JSON.stringify(outputData, null, 2));

        const fileSize = fs.statSync(outputPath).size;
        console.log("\n✅ ビギナー定跡データ生成完了");
        console.log(`📁 出力: ${outputPath}`);
        console.log(`📏 サイズ: ${(fileSize / 1024 / 1024).toFixed(2)} MB`);
    } catch (error) {
        console.error("\n❌ エラー:", error);
        process.exit(1);
    }
}

// 実行
if (import.meta.url === `file://${process.argv[1]}`) {
    generateBeginnerOpenings();
}

export { generateBeginnerOpenings };
