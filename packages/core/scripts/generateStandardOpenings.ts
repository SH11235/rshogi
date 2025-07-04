#!/usr/bin/env node
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import pako from "pako";
import type { OpeningEntry } from "../src/types/ai";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

/**
 * 標準的な序盤定跡を生成
 */
function generateStandardOpenings() {
    console.log("📚 標準定跡データ生成開始...");

    const entries: OpeningEntry[] = [];

    // 1. 初期局面
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -",
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 7 },
                    to: { row: 6, column: 7 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 40,
                name: "居飛車",
                comment: "最も一般的な初手",
            },
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 2 },
                    to: { row: 6, column: 2 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 30,
                name: "相掛かり",
                comment: "積極的な作戦",
            },
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 5 },
                    to: { row: 6, column: 5 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 20,
                name: "中飛車",
                comment: "振り飛車の一種",
            },
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 9 },
                    to: { row: 6, column: 9 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 10,
                name: "端歩",
                comment: "やや特殊な作戦",
            },
        ],
        depth: 0,
    });

    // 2. 76歩の後
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w -",
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 3 },
                    to: { row: 4, column: 3 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 50,
                name: "34歩",
                comment: "最も自然な応手",
            },
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 8 },
                    to: { row: 4, column: 8 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 30,
                name: "84歩",
                comment: "飛車先を突く",
            },
        ],
        depth: 1,
    });

    // 3. 26歩の後
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL w -",
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 8 },
                    to: { row: 4, column: 8 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 60,
                name: "84歩",
                comment: "飛車先を突く自然な手",
            },
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 3 },
                    to: { row: 4, column: 3 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 30,
                name: "34歩",
                comment: "角道を開ける",
            },
        ],
        depth: 1,
    });

    // 4. 76歩-34歩の後
    entries.push({
        position: "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b -",
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 2 },
                    to: { row: 6, column: 2 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 35,
                name: "26歩",
                comment: "矢倉戦法",
            },
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 6 },
                    to: { row: 6, column: 6 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 30,
                name: "66歩",
                comment: "四間飛車",
            },
            {
                move: {
                    type: "move",
                    from: { row: 8, column: 2 },
                    to: { row: 6, column: 2 },
                    piece: { type: "rook", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 20,
                name: "26飛",
                comment: "向かい飛車",
            },
            {
                move: {
                    type: "move",
                    from: { row: 8, column: 8 },
                    to: { row: 2, column: 2 },
                    piece: { type: "bishop", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 15,
                name: "22角成",
                comment: "角換わり",
            },
        ],
        depth: 2,
    });

    // 5. 26歩-84歩の後
    entries.push({
        position: "lnsgkgsnl/1r5b1/p1ppppppp/1p7/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL b -",
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 2 },
                    to: { row: 5, column: 2 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 60,
                name: "25歩",
                comment: "飛車先を伸ばす",
            },
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 7 },
                    to: { row: 6, column: 7 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 30,
                name: "76歩",
                comment: "角道を開ける",
            },
        ],
        depth: 2,
    });

    // 6. 26歩-84歩-25歩の後
    entries.push({
        position: "lnsgkgsnl/1r5b1/p1ppppppp/1p7/7P1/9/PPPPPPP1P/1B5R1/LNSGKGSNL w -",
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 8 },
                    to: { row: 5, column: 8 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 60,
                name: "85歩",
                comment: "飛車先を伸ばす",
            },
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 3 },
                    to: { row: 4, column: 3 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 30,
                name: "34歩",
                comment: "角道を開ける",
            },
        ],
        depth: 3,
    });

    // 7. 26歩-34歩-25歩の後（お尋ねの局面）
    entries.push({
        position: "lnsgkgsnl/1r5b1/pppppp1pp/6p2/7P1/9/PPPPPPP1P/1B5R1/LNSGKGSNL w -",
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 3 },
                    to: { row: 5, column: 3 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 50,
                name: "35歩",
                comment: "角道を活かす",
            },
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 8 },
                    to: { row: 4, column: 8 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 40,
                name: "84歩",
                comment: "飛車先を突く",
            },
        ],
        depth: 3,
    });

    // 8. 76歩-84歩-26歩の後（角換わりの定跡）
    entries.push({
        position: "lnsgkgsnl/1r5b1/p1ppppppp/1p7/9/2P4P1/PP1PPPP1P/1B5R1/LNSGKGSNL w -",
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 3 },
                    to: { row: 4, column: 3 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 60,
                name: "34歩",
                comment: "角換わりへ",
            },
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 8 },
                    to: { row: 5, column: 8 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 30,
                name: "85歩",
                comment: "飛車先を伸ばす",
            },
            {
                move: {
                    type: "move",
                    from: { row: 1, column: 4 },
                    to: { row: 2, column: 4 },
                    piece: { type: "king", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                },
                weight: 10,
                name: "42玉",
                comment: "玉を守る",
            },
        ],
        depth: 3,
    });

    // 統計情報
    const stats = {
        totalEntries: entries.length,
        byDepth: {} as Record<number, number>,
    };

    for (const entry of entries) {
        stats.byDepth[entry.depth] = (stats.byDepth[entry.depth] || 0) + 1;
    }

    // 出力データ
    const outputData = {
        version: "1.0.0",
        source: "Standard Openings Generator",
        entries,
    };

    // ファイルに保存
    const outputPath = path.join(
        __dirname,
        "../../web/public/data/openings/standard-openings.json",
    );
    const outputDir = path.dirname(outputPath);

    if (!fs.existsSync(outputDir)) {
        fs.mkdirSync(outputDir, { recursive: true });
    }

    // JSONを書き込み
    fs.writeFileSync(outputPath, JSON.stringify(outputData));

    // gzipで圧縮
    const jsonContent = fs.readFileSync(outputPath);
    const compressed = pako.gzip(jsonContent);
    fs.writeFileSync(`${outputPath}.gz`, compressed);

    // .gzip版も作成（Vite対応）
    fs.writeFileSync(outputPath.replace(".json", ".gzip"), compressed);

    // 元のJSONファイルを削除
    fs.unlinkSync(outputPath);

    console.log("✅ 標準定跡データ生成完了");
    console.log(`📊 総エントリ数: ${entries.length}`);
    console.log("📈 手数別分布:");
    for (let depth = 0; depth <= 10; depth++) {
        if (stats.byDepth[depth]) {
            console.log(`  ${depth}手目: ${stats.byDepth[depth]} 局面`);
        }
    }
}

// 実行
if (import.meta.url === `file://${process.argv[1]}`) {
    generateStandardOpenings();
}

export { generateStandardOpenings };
