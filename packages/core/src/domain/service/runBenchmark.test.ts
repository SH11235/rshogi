import { describe, expect, it } from "vitest";
import { MateSearchBenchmark, createBenchmarkPositions } from "./mateSearchBenchmark";

describe("Mate Search Performance Benchmark", () => {
    it("should run standard benchmark suite", () => {
        console.log("\n=== Running Mate Search Performance Benchmark ===\n");

        const benchmark = new MateSearchBenchmark();
        const positions = createBenchmarkPositions();

        // Run benchmark with depth 7
        const summary = benchmark.runBenchmarkSuite(positions, 7);

        // Format and display results
        const formattedResults = benchmark.formatResults(summary);
        console.log(formattedResults);

        // Verify benchmark ran successfully
        expect(summary.totalPositions).toBe(6);
        expect(summary.totalNodes).toBeGreaterThan(0);
        expect(summary.averageNodesPerSecond).toBeGreaterThan(0);

        // Log individual position results
        console.log("\n=== Detailed Position Analysis ===\n");
        for (const position of positions) {
            console.log(`\nAnalyzing: ${position.name}`);
            const depthResults = benchmark.runDepthAnalysis(position, 9);

            for (const result of depthResults) {
                console.log(
                    `  Depth ${result.depth}: ${result.nodeCount} nodes, ${result.elapsedMs}ms, ${result.nodesPerSecond} NPS`,
                );
                if (result.isMate) {
                    console.log(`  -> Mate found in ${result.moveCount} moves`);
                }
            }
        }
    });

    it("should compare performance across different depths", () => {
        console.log("\n=== Depth Comparison Benchmark ===\n");

        const benchmark = new MateSearchBenchmark();
        const positions = createBenchmarkPositions();
        const depths = [3, 5, 7];

        for (const depth of depths) {
            console.log(`\nTesting with max depth: ${depth}`);
            const summary = benchmark.runBenchmarkSuite(positions, depth);

            console.log(`  Total nodes: ${summary.totalNodes.toLocaleString()}`);
            console.log(`  Total time: ${summary.totalElapsedMs}ms`);
            console.log(`  Average NPS: ${summary.averageNodesPerSecond.toLocaleString()}`);

            // Count mate found
            const matesFound = summary.results.filter((r) => r.isMate).length;
            console.log(`  Mates found: ${matesFound}/${summary.totalPositions}`);
        }
    });
});
