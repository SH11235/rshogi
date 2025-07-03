#!/usr/bin/env tsx

import type { AIDifficulty } from "../src/types/ai";
import { AIBenchmark, measureOpeningBookUsage } from "./ai-benchmark";

async function runFullBenchmark() {
    console.log("🏁 Starting AI Performance Benchmark...\n");

    const benchmark = new AIBenchmark();

    // 全難易度でベンチマークを実行
    const results = await benchmark.runAllBenchmarks();

    // 結果をフォーマットして出力
    const formatted = benchmark.formatResults(results);
    console.log(`\n${formatted}`);

    // 定跡使用率の測定
    console.log("\n📚 Opening Book Usage Analysis:\n");

    const difficulties: AIDifficulty[] = ["beginner", "intermediate", "advanced", "expert"];
    for (const difficulty of difficulties) {
        console.log(`${difficulty.toUpperCase()}:`);
        const usage = await measureOpeningBookUsage(difficulty, 5);
        console.log(`  Total moves: ${usage.totalMoves}`);
        console.log(`  Book moves: ${usage.bookMoves}`);
        console.log(`  Book usage rate: ${usage.bookUsageRate.toFixed(1)}%`);
        console.log("");
    }

    // 性能比較サマリー
    console.log("\n📊 Performance Summary:\n");
    console.log("| Difficulty   | Avg Time (ms) | Avg Depth | Avg Nodes | Time Ratio |");
    console.log("|-------------|--------------|-----------|-----------|------------|");

    const beginnerTime = results.beginner.averageTime;
    for (const [difficulty, summary] of Object.entries(results)) {
        const timeRatio = (summary.averageTime / beginnerTime).toFixed(2);
        console.log(
            `| ${difficulty.padEnd(11)} | ${summary.averageTime.toFixed(1).padEnd(12)} | ${summary.averageDepth
                .toFixed(1)
                .padEnd(
                    9,
                )} | ${summary.averageNodes.toFixed(0).padEnd(9)} | ${`${timeRatio}x`.padEnd(11)} |`,
        );
    }

    // 結果をファイルに保存
    const fs = await import("node:fs/promises");

    // Markdownファイルに保存
    const mdFile = "benchmark-results.md";
    await fs.writeFile(mdFile, formatted);
    console.log(`\n✅ Results saved to ${mdFile}`);

    // JSONファイルにも保存（再利用や履歴管理のため）
    const jsonFile = `ai-benchmark-${new Date().toISOString().split("T")[0]}.json`;
    await fs.writeFile(jsonFile, JSON.stringify(results, null, 2));
    console.log(`✅ Raw data saved to ${jsonFile}`);
}

// CLIから実行された場合
if (import.meta.url === `file://${process.argv[1]}`) {
    runFullBenchmark()
        .then(() => {
            console.log("\n✨ Benchmark completed successfully!");
            process.exit(0);
        })
        .catch((error) => {
            console.error("\n❌ Benchmark failed:", error);
            process.exit(1);
        });
}

export { runFullBenchmark };
