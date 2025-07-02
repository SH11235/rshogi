import { createBenchmarkPositions } from "./mateSearchBenchmark";
import { MateSearchWasmService } from "./mateSearchWasm";

async function testWasmModule() {
    console.log("Testing WASM module...");

    try {
        const wasmService = new MateSearchWasmService();
        const positions = createBenchmarkPositions();
        const testPosition = positions[0]; // 1手詰め-頭金

        console.log(`Testing position: ${testPosition.name}`);

        const result = await wasmService.search(
            testPosition.board,
            testPosition.hands,
            testPosition.attacker,
            { maxDepth: 1 },
        );

        console.log("WASM Result:", {
            isMate: result.isMate,
            nodeCount: result.nodeCount,
            elapsedMs: result.elapsedMs,
        });

        wasmService.dispose();
        console.log("WASM test completed successfully!");
    } catch (error) {
        console.error("WASM test failed:", error);
    }
}

// Run test if called directly
if (import.meta.url === `file://${process.argv[1]}`) {
    testWasmModule();
}
