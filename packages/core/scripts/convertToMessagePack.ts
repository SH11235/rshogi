#!/usr/bin/env node
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { encode } from "@msgpack/msgpack";
import * as pako from "pako";
import {
    type MoveBinary,
    MoveFlags,
    type OpeningEntryBinary,
    PieceTypeBinary,
    coordinateToIndex,
} from "../src/ai/binaryOpeningTypes";
import type { Move, PieceType } from "../src/domain/model/move";
import type { OpeningEntry } from "../src/types/ai";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

/**
 * 駒種類をバイナリ形式に変換
 */
function pieceTypeToBinary(type: PieceType, promoted: boolean): number {
    const baseTypes: Record<PieceType, number> = {
        pawn: PieceTypeBinary.PAWN,
        lance: PieceTypeBinary.LANCE,
        knight: PieceTypeBinary.KNIGHT,
        silver: PieceTypeBinary.SILVER,
        gold: PieceTypeBinary.GOLD,
        bishop: PieceTypeBinary.BISHOP,
        rook: PieceTypeBinary.ROOK,
        king: PieceTypeBinary.KING,
        gyoku: PieceTypeBinary.KING,
    };

    if (promoted) {
        switch (type) {
            case "pawn":
                return PieceTypeBinary.PROMOTED_PAWN;
            case "lance":
                return PieceTypeBinary.PROMOTED_LANCE;
            case "knight":
                return PieceTypeBinary.PROMOTED_KNIGHT;
            case "silver":
                return PieceTypeBinary.PROMOTED_SILVER;
            case "bishop":
                return PieceTypeBinary.PROMOTED_BISHOP;
            case "rook":
                return PieceTypeBinary.PROMOTED_ROOK;
            default:
                return baseTypes[type];
        }
    }

    return baseTypes[type];
}

/**
 * 移動情報をバイナリ形式に変換
 */
function moveToBinary(move: Move, weight: number, evaluation: number): MoveBinary {
    let flags = 0;

    if (move.type === "drop") {
        // 駒打ち
        flags |= MoveFlags.DROP;
        flags |= pieceTypeToBinary(move.piece.type, false) & MoveFlags.PIECE_MASK;

        return {
            from: 0, // 駒打ちは0
            to: coordinateToIndex(move.to.row, move.to.column),
            flags,
            weight: Math.min(255, Math.max(0, weight)),
            eval: Math.min(32767, Math.max(-32768, evaluation)),
        };
    }
    // 通常の移動
    flags |= pieceTypeToBinary(move.piece.type, move.piece.promoted) & MoveFlags.PIECE_MASK;
    if (move.promote) {
        flags |= MoveFlags.PROMOTE;
    }

    return {
        from: coordinateToIndex(move.from.row, move.from.column),
        to: coordinateToIndex(move.to.row, move.to.column),
        flags,
        weight: Math.min(255, Math.max(0, weight)),
        eval: Math.min(32767, Math.max(-32768, evaluation)),
    };
}

/**
 * JSONエントリーをバイナリ形式に変換
 */
function convertEntry(entry: OpeningEntry): OpeningEntryBinary {
    const moves: MoveBinary[] = entry.moves.map((om) => {
        // コメントから評価値を抽出
        const evalMatch = om.comment?.match(/評価値:\s*([+-]?\d+)/);
        const evaluation = evalMatch ? Number.parseInt(evalMatch[1], 10) : 0;

        return moveToBinary(om.move, om.weight, evaluation);
    });

    return {
        positionHash: entry.position,
        depth: Math.min(255, entry.depth),
        moves,
    };
}

/**
 * ファイルを変換
 */
async function convertFile(inputPath: string, outputPath: string): Promise<void> {
    console.log(`📄 変換中: ${path.basename(inputPath)}`);

    // gzipファイルを読み込み
    const compressed = fs.readFileSync(inputPath);
    const jsonData = pako.ungzip(compressed, { to: "string" });
    const data = JSON.parse(jsonData);

    // バイナリ形式に変換
    const binaryEntries: OpeningEntryBinary[] = data.entries.map(convertEntry);

    // MessagePackでエンコード
    const encoded = encode({
        version: "2.0.0",
        format: "msgpack",
        entries: binaryEntries,
    });

    // gzipで圧縮
    const compressedBinary = pako.gzip(encoded);

    // ファイルに保存
    fs.writeFileSync(outputPath, compressedBinary);

    // サイズ比較
    const originalSize = compressed.length;
    const newSize = compressedBinary.length;
    const reduction = ((1 - newSize / originalSize) * 100).toFixed(1);

    console.log(`  元サイズ: ${(originalSize / 1024 / 1024).toFixed(2)} MB`);
    console.log(`  新サイズ: ${(newSize / 1024 / 1024).toFixed(2)} MB`);
    console.log(`  削減率: ${reduction}%`);
}

/**
 * メイン処理
 */
async function main() {
    const inputDir = path.join(__dirname, "../../web/public/data/openings");
    const outputDir = path.join(__dirname, "../../web/public/data/openings-msgpack");

    // 出力ディレクトリ作成
    if (!fs.existsSync(outputDir)) {
        fs.mkdirSync(outputDir, { recursive: true });
    }

    // すべての.json.gzファイルを変換
    const files = fs.readdirSync(inputDir).filter((f) => f.endsWith(".json.gz"));

    console.log(`🔄 ${files.length} ファイルを変換します...`);

    let totalOriginalSize = 0;
    let totalNewSize = 0;

    for (const file of files) {
        const inputPath = path.join(inputDir, file);
        const outputPath = path.join(outputDir, file.replace(".json.gz", ".msgpack.gz"));

        await convertFile(inputPath, outputPath);

        totalOriginalSize += fs.statSync(inputPath).size;
        totalNewSize += fs.statSync(outputPath).size;
    }

    // 新しいインデックスファイルを作成
    const indexData = {
        version: "2.0.0",
        format: "msgpack",
        createdAt: new Date().toISOString(),
        totalEntries: 806981, // 既存の値を使用
        files: files.map((f) => ({
            name: f.replace(".json.gz", ".msgpack.gz"),
            size: fs.statSync(path.join(outputDir, f.replace(".json.gz", ".msgpack.gz"))).size,
        })),
    };

    fs.writeFileSync(path.join(outputDir, "index.json"), JSON.stringify(indexData, null, 2));

    console.log("\n✅ 変換完了！");
    console.log("📊 総計:");
    console.log(`  元サイズ: ${(totalOriginalSize / 1024 / 1024).toFixed(2)} MB`);
    console.log(`  新サイズ: ${(totalNewSize / 1024 / 1024).toFixed(2)} MB`);
    console.log(`  削減率: ${((1 - totalNewSize / totalOriginalSize) * 100).toFixed(1)}%`);
}

// 実行
if (import.meta.url === `file://${process.argv[1]}`) {
    main().catch(console.error);
}

export { convertFile, main };
