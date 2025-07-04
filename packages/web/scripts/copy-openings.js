import { copyFileSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const sourceDir = join(__dirname, "../../core/data/openings");
const targetDir = join(__dirname, "../public/data/openings");

// ターゲットディレクトリを作成
mkdirSync(targetDir, { recursive: true });

// ソースディレクトリが存在するか確認
if (!existsSync(sourceDir)) {
    console.error(`Source directory does not exist: ${sourceDir}`);
    process.exit(1);
}

// ファイルをコピー
const files = readdirSync(sourceDir);
let copiedCount = 0;

for (const file of files) {
    // .gzip と .json ファイルのみコピー
    if (file.endsWith(".gzip") || file.endsWith(".json")) {
        const sourcePath = join(sourceDir, file);
        const targetPath = join(targetDir, file);

        try {
            copyFileSync(sourcePath, targetPath);
            copiedCount++;
            console.log(`Copied: ${file}`);
        } catch (error) {
            console.error(`Failed to copy ${file}:`, error.message);
        }
    }
}

console.log(`\n✅ Copied ${copiedCount} files from core/data/openings to web/public/data/openings`);
