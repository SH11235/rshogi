import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
    type BenchmarkSummary,
    MateSearchBenchmark,
    createBenchmarkPositions,
} from "./mateSearchBenchmark";

interface BenchmarkOutput {
    timestamp: string;
    implementation: "javascript" | "rust-wasm";
    summary: BenchmarkSummary;
    environment?: {
        nodeVersion?: string;
        platform?: string;
    };
}

/**
 * シンプルなベンチマーク実行とファイル出力
 */
export function runAndSaveBenchmark(
    implementation: "javascript" | "rust-wasm" = "javascript",
): void {
    const benchmark = new MateSearchBenchmark();
    const positions = createBenchmarkPositions();

    console.log(`Running ${implementation} benchmark...`);
    const summary = benchmark.runBenchmarkSuite(positions, 7);

    // 結果を整形して表示
    const formatted = benchmark.formatResults(summary);
    console.log(formatted);

    // benchmarksディレクトリを作成
    const benchmarkDir = join(process.cwd(), "benchmarks");
    if (!existsSync(benchmarkDir)) {
        mkdirSync(benchmarkDir, { recursive: true });
    }

    // 結果をJSONファイルに保存
    const output: BenchmarkOutput = {
        timestamp: new Date().toISOString(),
        implementation,
        summary,
        environment: {
            nodeVersion: process.version,
            platform: process.platform,
        },
    };

    const filename = `mate-search-${implementation}-${Date.now()}.benchmark.json`;
    const filepath = join(benchmarkDir, filename);

    writeFileSync(filepath, JSON.stringify(output, null, 2));
    console.log(`\nBenchmark results saved to: ${filepath}`);
}

// CLIから実行する場合
// ESモジュールでのエントリーポイント判定
const __filename = fileURLToPath(import.meta.url);

if (process.argv[1] === __filename) {
    // コマンドライン引数で実装を選択
    const implementation = process.argv[2] === "wasm" ? "rust-wasm" : "javascript";
    runAndSaveBenchmark(implementation);
}
