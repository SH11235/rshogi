import { AIEngine } from "../src/ai/engine";
import { generateMainOpenings } from "../src/ai/openingGenerator";
import { modernInitialBoard } from "../src/domain/initialBoard";
import type { Board } from "../src/domain/model/board";
import type { Move } from "../src/domain/model/move";
import { initialHands } from "../src/domain/service/moveService";
import type { AIDifficulty } from "../src/types/ai";

export interface BenchmarkResult {
    difficulty: AIDifficulty;
    position: string;
    moveTime: number; // ミリ秒
    nodesSearched: number;
    depth: number;
    evaluation: number;
    bestMove: Move;
    pv: Move[];
}

export interface BenchmarkSummary {
    difficulty: AIDifficulty;
    totalPositions: number;
    averageTime: number;
    minTime: number;
    maxTime: number;
    averageDepth: number;
    averageNodes: number;
    results: BenchmarkResult[];
}

// ベンチマーク用の局面
export const BENCHMARK_POSITIONS = [
    {
        name: "初期局面",
        board: modernInitialBoard,
        hands: initialHands(),
        player: "black" as const,
        moveHistory: [],
    },
    {
        name: "中盤の局面",
        board: (() => {
            const board = {} as Board;
            // 簡略化した中盤の局面
            board["19"] = { type: "lance", owner: "black", promoted: false };
            board["29"] = { type: "knight", owner: "black", promoted: false };
            board["39"] = { type: "silver", owner: "black", promoted: false };
            board["49"] = { type: "gold", owner: "black", promoted: false };
            board["59"] = { type: "king", owner: "black", promoted: false };
            board["69"] = { type: "gold", owner: "black", promoted: false };
            board["79"] = { type: "silver", owner: "black", promoted: false };
            board["89"] = { type: "knight", owner: "black", promoted: false };
            board["99"] = { type: "lance", owner: "black", promoted: false };
            board["28"] = { type: "rook", owner: "black", promoted: false };
            board["88"] = { type: "bishop", owner: "black", promoted: false };
            board["17"] = { type: "pawn", owner: "black", promoted: false };
            board["37"] = { type: "pawn", owner: "black", promoted: false };
            board["47"] = { type: "pawn", owner: "black", promoted: false };
            board["57"] = { type: "pawn", owner: "black", promoted: false };
            board["67"] = { type: "pawn", owner: "black", promoted: false };
            board["77"] = { type: "pawn", owner: "black", promoted: false };
            board["97"] = { type: "pawn", owner: "black", promoted: false };

            board["11"] = { type: "lance", owner: "white", promoted: false };
            board["21"] = { type: "knight", owner: "white", promoted: false };
            board["31"] = { type: "silver", owner: "white", promoted: false };
            board["41"] = { type: "gold", owner: "white", promoted: false };
            board["51"] = { type: "gyoku", owner: "white", promoted: false };
            board["61"] = { type: "gold", owner: "white", promoted: false };
            board["71"] = { type: "silver", owner: "white", promoted: false };
            board["81"] = { type: "knight", owner: "white", promoted: false };
            board["91"] = { type: "lance", owner: "white", promoted: false };
            board["22"] = { type: "bishop", owner: "white", promoted: false };
            board["82"] = { type: "rook", owner: "white", promoted: false };
            board["13"] = { type: "pawn", owner: "white", promoted: false };
            board["33"] = { type: "pawn", owner: "white", promoted: false };
            board["43"] = { type: "pawn", owner: "white", promoted: false };
            board["53"] = { type: "pawn", owner: "white", promoted: false };
            board["63"] = { type: "pawn", owner: "white", promoted: false };
            board["73"] = { type: "pawn", owner: "white", promoted: false };
            board["93"] = { type: "pawn", owner: "white", promoted: false };

            return board;
        })(),
        hands: (() => {
            const hands = initialHands();
            // 中盤らしく、いくつか駒を持ち駒に
            hands.black.pawn = 1;
            hands.white.pawn = 1;
            return hands;
        })(),
        player: "black" as const,
        moveHistory: [],
    },
    {
        name: "終盤の局面",
        board: (() => {
            const board = {} as Board;
            // 簡略化した終盤の局面
            board["59"] = { type: "king", owner: "black", promoted: false };
            board["49"] = { type: "gold", owner: "black", promoted: false };
            board["69"] = { type: "gold", owner: "black", promoted: false };
            board["58"] = { type: "silver", owner: "black", promoted: false };
            board["28"] = { type: "rook", owner: "black", promoted: true }; // 龍

            board["51"] = { type: "gyoku", owner: "white", promoted: false };
            board["41"] = { type: "gold", owner: "white", promoted: false };
            board["52"] = { type: "silver", owner: "white", promoted: false };

            return board;
        })(),
        hands: (() => {
            const hands = initialHands();
            // 終盤らしい持ち駒
            hands.black.pawn = 3;
            hands.black.silver = 1;
            hands.white.pawn = 2;
            hands.white.gold = 1;
            hands.white.bishop = 1;
            return hands;
        })(),
        player: "black" as const,
        moveHistory: [],
    },
];

export class AIBenchmark {
    async runBenchmark(difficulty: AIDifficulty): Promise<BenchmarkSummary> {
        const engine = new AIEngine(difficulty);
        // Load opening book for appropriate difficulties
        if (difficulty !== "beginner") {
            const openingData = generateMainOpenings();
            engine.loadOpeningBook(openingData);
        }
        const results: BenchmarkResult[] = [];

        console.log(`Running benchmark for ${difficulty}...`);

        for (const position of BENCHMARK_POSITIONS) {
            console.log(`Testing position: ${position.name}`);

            const startTime = performance.now();

            try {
                const bestMove = await engine.calculateBestMove(
                    position.board,
                    position.hands,
                    position.player,
                    position.moveHistory,
                );

                const endTime = performance.now();
                const moveTime = endTime - startTime;

                const evaluation = engine.getLastEvaluation();

                results.push({
                    difficulty,
                    position: position.name,
                    moveTime,
                    nodesSearched: evaluation.nodes,
                    depth: evaluation.depth,
                    evaluation: evaluation.score,
                    bestMove,
                    pv: evaluation.pv,
                });

                console.log(
                    `  Time: ${moveTime.toFixed(2)}ms, Depth: ${evaluation.depth}, Nodes: ${evaluation.nodes}`,
                );
            } catch (error) {
                console.error(`Error in position ${position.name}:`, error);
            }
        }

        // 統計情報を計算
        const times = results.map((r) => r.moveTime);
        const depths = results.map((r) => r.depth);
        const nodes = results.map((r) => r.nodesSearched);

        const summary: BenchmarkSummary = {
            difficulty,
            totalPositions: results.length,
            averageTime: times.reduce((a, b) => a + b, 0) / times.length,
            minTime: Math.min(...times),
            maxTime: Math.max(...times),
            averageDepth: depths.reduce((a, b) => a + b, 0) / depths.length,
            averageNodes: nodes.reduce((a, b) => a + b, 0) / nodes.length,
            results,
        };

        return summary;
    }

    async runAllBenchmarks(): Promise<Record<AIDifficulty, BenchmarkSummary>> {
        const difficulties: AIDifficulty[] = ["beginner", "intermediate", "advanced", "expert"];
        const allResults: Record<AIDifficulty, BenchmarkSummary> = {} as Record<
            AIDifficulty,
            BenchmarkSummary
        >;

        for (const difficulty of difficulties) {
            allResults[difficulty] = await this.runBenchmark(difficulty);
        }

        return allResults;
    }

    formatResults(results: Partial<Record<AIDifficulty, BenchmarkSummary>>): string {
        let output = "# AI Performance Benchmark Results\n\n";
        output += `Generated at: ${new Date().toISOString()}\n\n`;

        for (const [difficulty, summary] of Object.entries(results)) {
            if (!summary) continue;

            output += `## ${difficulty.toUpperCase()}\n\n`;
            output += `- Total Positions: ${summary.totalPositions}\n`;
            output += `- Average Time: ${summary.averageTime.toFixed(2)}ms\n`;
            output += `- Min Time: ${summary.minTime.toFixed(2)}ms\n`;
            output += `- Max Time: ${summary.maxTime.toFixed(2)}ms\n`;
            output += `- Average Depth: ${summary.averageDepth.toFixed(1)}\n`;
            output += `- Average Nodes: ${summary.averageNodes.toFixed(0)}\n\n`;

            output += "### Position Details:\n";
            for (const result of summary.results) {
                output += `- **${result.position}**: ${result.moveTime.toFixed(2)}ms, `;
                output += `Depth ${result.depth}, ${result.nodesSearched} nodes, `;
                output += `Eval ${result.evaluation}\n`;
            }
            output += "\n";
        }

        return output;
    }
}

// 定跡使用率の測定
export async function measureOpeningBookUsage(
    difficulty: AIDifficulty,
    numGames = 10,
): Promise<{
    totalMoves: number;
    bookMoves: number;
    bookUsageRate: number;
}> {
    const engine = new AIEngine(difficulty);
    // Load opening book for appropriate difficulties
    if (difficulty !== "beginner") {
        const openingData = generateMainOpenings();
        engine.loadOpeningBook(openingData);
    }
    let totalMoves = 0;
    let bookMoves = 0;

    for (let game = 0; game < numGames; game++) {
        const board = modernInitialBoard;
        const hands = initialHands();
        const moveHistory: Move[] = [];

        // 最初の20手まで測定
        for (let moveNum = 0; moveNum < 20 && moveNum < 40; moveNum++) {
            const player = moveNum % 2 === 0 ? "black" : "white";

            try {
                const move = await engine.calculateBestMove(
                    board,
                    hands,
                    player as "black" | "white",
                    moveHistory,
                );
                const evaluation = engine.getLastEvaluation();

                totalMoves++;
                // 深さ0は定跡からの手
                if (evaluation.depth === 0) {
                    bookMoves++;
                }

                // 実際に手を指す処理（省略）
                moveHistory.push(move);
            } catch (error) {
                break;
            }
        }
    }

    return {
        totalMoves,
        bookMoves,
        bookUsageRate: totalMoves > 0 ? (bookMoves / totalMoves) * 100 : 0,
    };
}
