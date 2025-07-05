import { describe, expect, it } from "vitest";
import { AIBenchmark, BENCHMARK_POSITIONS } from "./ai-benchmark";

describe("AI Benchmark", () => {
    it("should run benchmark for beginner difficulty", async () => {
        const benchmark = new AIBenchmark();
        const result = await benchmark.runBenchmark("beginner");

        expect(result.difficulty).toBe("beginner");
        expect(result.totalPositions).toBe(BENCHMARK_POSITIONS.length);
        expect(result.averageTime).toBeGreaterThan(0);
        expect(result.averageTime).toBeLessThan(2000); // 初級は2秒以内
        expect(result.averageDepth).toBeGreaterThanOrEqual(1);
        expect(result.averageDepth).toBeLessThanOrEqual(3);
        expect(result.results).toHaveLength(BENCHMARK_POSITIONS.length);

        // 各局面の結果を確認
        for (const positionResult of result.results) {
            expect(positionResult.moveTime).toBeGreaterThan(0);
            expect(positionResult.depth).toBeGreaterThan(0);
            expect(positionResult.nodesSearched).toBeGreaterThan(0);
            expect(positionResult.bestMove).toBeDefined();
        }
    }, 30000); // 30秒のタイムアウト

    it("should show increasing depth and time with difficulty", async () => {
        const benchmark = new AIBenchmark();

        // 初級と中級を比較
        const beginnerResult = await benchmark.runBenchmark("beginner");
        const intermediateResult = await benchmark.runBenchmark("intermediate");

        // 中級の方が深く探索する
        expect(intermediateResult.averageDepth).toBeGreaterThan(beginnerResult.averageDepth);
        // 中級の方が時間がかかる
        expect(intermediateResult.averageTime).toBeGreaterThan(beginnerResult.averageTime);
        // 中級の方が多くのノードを探索する
        expect(intermediateResult.averageNodes).toBeGreaterThan(beginnerResult.averageNodes);
    }, 60000); // 60秒のタイムアウト

    it("should format benchmark results correctly", async () => {
        const benchmark = new AIBenchmark();
        const result = await benchmark.runBenchmark("beginner");

        const formatted = benchmark.formatResults({ beginner: result });

        expect(formatted).toContain("# AI Performance Benchmark Results");
        expect(formatted).toContain("BEGINNER");
        expect(formatted).toContain("Average Time:");
        expect(formatted).toContain("Average Depth:");
        expect(formatted).toContain("Position Details:");
        expect(formatted).toContain("初期局面");
    }, 30000);
});

// パフォーマンス比較テスト
describe("AI Performance Comparison", () => {
    it("should demonstrate performance characteristics", async () => {
        const benchmark = new AIBenchmark();
        const difficulties: Array<"beginner" | "intermediate"> = ["beginner", "intermediate"];
        const results: Record<string, { avgTime: number; avgDepth: number; avgNodes: number }> = {};

        for (const difficulty of difficulties) {
            const result = await benchmark.runBenchmark(difficulty);
            results[difficulty] = {
                avgTime: result.averageTime,
                avgDepth: result.averageDepth,
                avgNodes: result.averageNodes,
            };

            console.log(`\n${difficulty.toUpperCase()} Performance:`);
            console.log(`  Average Time: ${result.averageTime.toFixed(2)}ms`);
            console.log(`  Average Depth: ${result.averageDepth.toFixed(1)}`);
            console.log(`  Average Nodes: ${result.averageNodes.toFixed(0)}`);
        }

        // 期待される性能特性を確認
        expect(results.intermediate.avgDepth).toBeGreaterThanOrEqual(results.beginner.avgDepth);
        expect(results.intermediate.avgTime).toBeGreaterThan(results.beginner.avgTime);
    }, 120000); // 2分のタイムアウト
});
