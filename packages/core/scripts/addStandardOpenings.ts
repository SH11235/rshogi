import * as fs from "node:fs";
import * as path from "node:path";
import pako from "pako";
import { generateMainOpenings } from "../src/ai/openingGenerator";

// インデックスファイルの型定義
interface IndexFile {
    name: string;
    size: number;
}

interface IndexData {
    version: string;
    createdAt: string;
    totalEntries: number;
    files: IndexFile[];
}

/**
 * 標準的な定跡を生成してファイルに追加
 */
async function addStandardOpenings() {
    console.log("標準定跡の追加を開始します...");

    // 標準的な定跡を生成
    const standardOpenings = generateMainOpenings();
    console.log(`生成された定跡数: ${standardOpenings.length}`);

    // 出力ディレクトリ（ルートディレクトリに既存のファイルがある）
    const outputDir = process.cwd();

    // 標準定跡用のファイルを作成
    const standardOpeningsFile = {
        version: "1.0.0",
        source: "Standard Openings Generator",
        entries: standardOpenings,
    };

    const jsonData = JSON.stringify(standardOpeningsFile);
    const compressedData = pako.gzip(jsonData);

    // standard-openings.json.gz として保存
    const outputPath = path.join(outputDir, "standard-openings.json.gz");
    fs.writeFileSync(outputPath, compressedData);
    console.log(`標準定跡ファイルを作成: ${outputPath}`);
    console.log(`ファイルサイズ: ${compressedData.length} bytes`);

    // index.json を更新
    const indexPath = path.join(outputDir, "index.json");
    let indexData: IndexData;

    if (fs.existsSync(indexPath)) {
        indexData = JSON.parse(fs.readFileSync(indexPath, "utf-8"));
    } else {
        indexData = {
            version: "1.0.0",
            createdAt: new Date().toISOString(),
            totalEntries: 0,
            files: [],
        };
    }

    // 標準定跡ファイルをインデックスの先頭に追加
    const standardFileInfo = {
        name: "standard-openings.json.gz",
        size: compressedData.length,
    };

    // 既存のエントリを削除（重複を避ける）
    indexData.files = indexData.files.filter((f) => f.name !== "standard-openings.json.gz");

    // 先頭に追加
    indexData.files.unshift(standardFileInfo);

    // 総エントリ数を更新
    indexData.totalEntries = indexData.files.reduce((total: number, file) => {
        if (file.name === "standard-openings.json.gz") {
            return total + standardOpenings.length;
        }
        // 他のファイルのエントリ数は変わらない
        return total;
    }, 806978); // 既存の総数

    fs.writeFileSync(indexPath, JSON.stringify(indexData, null, 4));
    console.log("index.json を更新しました");

    console.log("\n✅ 標準定跡の追加が完了しました！");
    console.log("次の手順:");
    console.log("1. cp standard-openings.json.gz packages/web/public/data/openings/");
    console.log("2. cp standard-openings.json.gz packages/web/dist/data/openings/");
    console.log("3. ブラウザでテスト");
}

// 実行
addStandardOpenings().catch(console.error);
