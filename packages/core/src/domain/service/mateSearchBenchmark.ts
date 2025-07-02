import type { Board } from "../model/board";
import type { Piece } from "../model/piece";
import type { SquareKey } from "../model/square";
import { MateSearchService } from "./mateSearch";
import type { Hands } from "./moveService";
import { initialHands } from "./moveService";

export interface BenchmarkPosition {
    name: string;
    description: string;
    board: Board;
    hands: Hands;
    attacker: "black" | "white";
    expectedDepth: number; // 期待される詰み手数
}

export interface BenchmarkResult {
    position: string;
    depth: number;
    nodeCount: number;
    elapsedMs: number;
    nodesPerSecond: number;
    isMate: boolean;
    moveCount: number;
}

export interface BenchmarkSummary {
    totalPositions: number;
    totalNodes: number;
    totalElapsedMs: number;
    averageNodesPerSecond: number;
    results: BenchmarkResult[];
}

/**
 * ベンチマーク用の標準的な詰将棋問題集
 */
export function createBenchmarkPositions(): BenchmarkPosition[] {
    const positions: BenchmarkPosition[] = [];

    // 1手詰めベンチマーク
    positions.push({
        name: "1手詰め-頭金",
        description: "簡単な頭金の1手詰め",
        board: createBoardFromPosition([
            { square: "11", piece: { type: "king", owner: "white", promoted: false } },
            { square: "21", piece: { type: "gold", owner: "black", promoted: false } },
            { square: "22", piece: { type: "gold", owner: "black", promoted: false } },
            { square: "99", piece: { type: "king", owner: "black", promoted: false } },
        ]),
        hands: {
            black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 1, 角: 0, 飛: 0 },
            white: { ...initialHands().white },
        },
        attacker: "black",
        expectedDepth: 1,
    });

    positions.push({
        name: "1手詰め-飛車",
        description: "飛車による1手詰め",
        board: createBoardFromPosition([
            { square: "51", piece: { type: "king", owner: "white", promoted: false } },
            { square: "59", piece: { type: "rook", owner: "black", promoted: false } },
            { square: "99", piece: { type: "king", owner: "black", promoted: false } },
        ]),
        hands: initialHands(),
        attacker: "black",
        expectedDepth: 1,
    });

    // 3手詰めベンチマーク
    positions.push({
        name: "3手詰め-銀と金",
        description: "銀と金による3手詰め",
        board: createBoardFromPosition([
            { square: "11", piece: { type: "king", owner: "white", promoted: false } },
            { square: "23", piece: { type: "silver", owner: "black", promoted: false } },
            { square: "99", piece: { type: "king", owner: "black", promoted: false } },
        ]),
        hands: {
            black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 1, 角: 0, 飛: 0 },
            white: { ...initialHands().white },
        },
        attacker: "black",
        expectedDepth: 3,
    });

    positions.push({
        name: "3手詰め-飛車と金",
        description: "飛車で王手して金で詰める",
        board: createBoardFromPosition([
            { square: "51", piece: { type: "king", owner: "white", promoted: false } },
            { square: "53", piece: { type: "rook", owner: "black", promoted: false } },
            { square: "99", piece: { type: "king", owner: "black", promoted: false } },
        ]),
        hands: {
            black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 1, 角: 0, 飛: 0 },
            white: { ...initialHands().white },
        },
        attacker: "black",
        expectedDepth: 3,
    });

    // 5手詰めベンチマーク
    positions.push({
        name: "5手詰め-複雑",
        description: "複雑な5手詰め",
        board: createBoardFromPosition([
            { square: "13", piece: { type: "king", owner: "white", promoted: false } },
            { square: "15", piece: { type: "rook", owner: "black", promoted: false } },
            { square: "35", piece: { type: "silver", owner: "black", promoted: false } },
            { square: "99", piece: { type: "king", owner: "black", promoted: false } },
        ]),
        hands: {
            black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 1, 角: 0, 飛: 0 },
            white: { ...initialHands().white },
        },
        attacker: "black",
        expectedDepth: 5,
    });

    // 詰みなしベンチマーク（探索性能測定用）
    positions.push({
        name: "詰みなし-中盤",
        description: "中盤の局面で詰みなし",
        board: createBoardFromPosition([
            { square: "59", piece: { type: "king", owner: "black", promoted: false } },
            { square: "51", piece: { type: "king", owner: "white", promoted: false } },
            { square: "55", piece: { type: "rook", owner: "black", promoted: false } },
            { square: "45", piece: { type: "bishop", owner: "white", promoted: false } },
        ]),
        hands: {
            black: { 歩: 2, 香: 0, 桂: 1, 銀: 0, 金: 1, 角: 0, 飛: 0 },
            white: { 歩: 3, 香: 1, 桂: 0, 銀: 1, 金: 0, 角: 0, 飛: 0 },
        },
        attacker: "black",
        expectedDepth: 0, // 詰みなし
    });

    return positions;
}

/**
 * 局面記述から盤面を作成するヘルパー関数
 */
function createBoardFromPosition(pieces: { square: string; piece: Piece }[]): Board {
    const board: Board = {} as Board;
    // 空の盤面を初期化
    for (let row = 1; row <= 9; row++) {
        for (let col = 1; col <= 9; col++) {
            board[`${row}${col}` as SquareKey] = null;
        }
    }
    // 駒を配置
    for (const { square, piece } of pieces) {
        board[square as SquareKey] = piece;
    }
    return board;
}

/**
 * ベンチマークを実行
 */
export class MateSearchBenchmark {
    private service: MateSearchService;

    constructor() {
        this.service = new MateSearchService();
    }

    /**
     * 単一の局面でベンチマークを実行
     */
    public runSingle(position: BenchmarkPosition, maxDepth = 7): BenchmarkResult {
        const startTime = Date.now();

        const result = this.service.search(position.board, position.hands, position.attacker, {
            maxDepth,
        });

        const elapsedMs = Date.now() - startTime;
        const nodesPerSecond =
            result.nodeCount > 0 && elapsedMs > 0
                ? Math.round((result.nodeCount / elapsedMs) * 1000)
                : 0;

        return {
            position: position.name,
            depth: maxDepth,
            nodeCount: result.nodeCount,
            elapsedMs,
            nodesPerSecond,
            isMate: result.isMate,
            moveCount: result.moves.length,
        };
    }

    /**
     * 複数の局面でベンチマークを実行
     */
    public runBenchmarkSuite(positions: BenchmarkPosition[], maxDepth = 7): BenchmarkSummary {
        const results: BenchmarkResult[] = [];
        let totalNodes = 0;
        let totalElapsedMs = 0;

        for (const position of positions) {
            console.log(`Running benchmark: ${position.name}`);
            const result = this.runSingle(position, maxDepth);
            results.push(result);
            totalNodes += result.nodeCount;
            totalElapsedMs += result.elapsedMs;

            console.log(
                `  - Nodes: ${result.nodeCount}, Time: ${result.elapsedMs}ms, NPS: ${result.nodesPerSecond}`,
            );
            console.log(`  - Result: ${result.isMate ? `Mate in ${result.moveCount}` : "No mate"}`);
        }

        const averageNodesPerSecond =
            totalNodes > 0 && totalElapsedMs > 0
                ? Math.round((totalNodes / totalElapsedMs) * 1000)
                : 0;

        return {
            totalPositions: positions.length,
            totalNodes,
            totalElapsedMs,
            averageNodesPerSecond,
            results,
        };
    }

    /**
     * 深さごとのパフォーマンス測定
     */
    public runDepthAnalysis(position: BenchmarkPosition, maxDepth = 9): BenchmarkResult[] {
        const results: BenchmarkResult[] = [];

        for (let depth = 1; depth <= maxDepth; depth += 2) {
            console.log(`Testing depth ${depth}`);
            const result = this.runSingle(position, depth);
            results.push(result);

            // 詰みが見つかったら終了
            if (result.isMate) {
                console.log(`Mate found at depth ${depth}`);
                break;
            }
        }

        return results;
    }

    /**
     * ベンチマーク結果を比較用のフォーマットで出力
     */
    public formatResults(summary: BenchmarkSummary): string {
        const lines: string[] = [];

        lines.push("=== Mate Search Benchmark Results ===");
        lines.push(`Total Positions: ${summary.totalPositions}`);
        lines.push(`Total Nodes: ${summary.totalNodes.toLocaleString()}`);
        lines.push(`Total Time: ${summary.totalElapsedMs}ms`);
        lines.push(`Average NPS: ${summary.averageNodesPerSecond.toLocaleString()}`);
        lines.push("");
        lines.push("Position Results:");

        for (const result of summary.results) {
            lines.push(`- ${result.position}`);
            lines.push(`  Nodes: ${result.nodeCount.toLocaleString()}`);
            lines.push(`  Time: ${result.elapsedMs}ms`);
            lines.push(`  NPS: ${result.nodesPerSecond.toLocaleString()}`);
            lines.push(`  Result: ${result.isMate ? `Mate in ${result.moveCount}` : "No mate"}`);
        }

        return lines.join("\n");
    }
}

/**
 * 標準ベンチマークを実行するヘルパー関数
 */
export function runStandardBenchmark(maxDepth = 7): BenchmarkSummary {
    const benchmark = new MateSearchBenchmark();
    const positions = createBenchmarkPositions();
    return benchmark.runBenchmarkSuite(positions, maxDepth);
}
