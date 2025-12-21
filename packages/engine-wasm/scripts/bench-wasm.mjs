import { readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import initWasm, {
    init as initEngine,
    load_model as loadModel,
    load_position as loadPosition,
    search as runSearch,
    set_event_handler as setEventHandler,
} from "../pkg/engine_wasm.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const DEFAULT_POSITIONS = [
    {
        name: "hirate-like",
        sfen: "lnsgkgsnl/1r7/p1ppp1bpp/1p3pp2/7P1/2P6/PP1PPPP1P/1B3S1R1/LNSGKG1NL b - 9",
    },
    {
        name: "complex-middle",
        sfen: "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
    },
    {
        name: "tactical",
        sfen: "6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1",
    },
    {
        name: "movegen-heavy",
        sfen: "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
    },
];

const usage = () =>
    `
Usage: node ./scripts/bench-wasm.mjs --nnue-file <path> [options]

Options:
  --nnue-file <path>   NNUE model file (local path).
  --material           Material評価のみで計測（NNUEを読み込まない）。
  --wasm <path>        Optional. Path to engine_wasm_bg.wasm.
  --sfens <path>       Optional. SFEN list file (name | sfen per line).
  --sfen <sfen>        Optional. Single position. Default: startpos.
  --moves "<m1 m2>"    Optional. Space/comma separated moves (single position only).
  --nodes <n>          Optional. Default: 1000000.
  --movetime-ms <ms>   Optional. If set, overrides --nodes.
  --iterations <n>     Optional. Default: 1. (--runs is an alias)
  --runs <n>           Optional. Alias for --iterations.
  --warmup <n>         Optional. Default: 0.
  --tt-size-mb <n>     Optional. Default: 64.
  --multi-pv <n>       Optional. Default: 1.
`.trim();

const parseIntArg = (value, label) => {
    const parsed = Number(value);
    if (!Number.isFinite(parsed) || parsed <= 0) {
        throw new Error(`${label} must be a positive number`);
    }
    return Math.floor(parsed);
};

const parseMoves = (value) =>
    value
        .split(/[,\s]+/g)
        .map((v) => v.trim())
        .filter(Boolean);

const parseArgs = (argv) => {
    const args = {
        useNnue: true,
        nodes: 1_000_000,
        iterations: 1,
        warmup: 0,
        ttSizeMb: 64,
        multiPv: 1,
        moves: [],
    };

    for (let i = 0; i < argv.length; i += 1) {
        const arg = argv[i];
        if (arg === "--") {
            continue;
        }
        switch (arg) {
            case "--nnue-file":
                args.nnueFile = argv[++i];
                break;
            case "--material":
                args.useNnue = false;
                break;
            case "--wasm":
                args.wasmPath = argv[++i];
                break;
            case "--sfens":
                args.sfensPath = argv[++i];
                break;
            case "--sfen":
                args.sfen = argv[++i];
                break;
            case "--moves":
                args.moves = parseMoves(argv[++i] ?? "");
                break;
            case "--nodes":
                args.nodes = parseIntArg(argv[++i], "nodes");
                break;
            case "--movetime-ms":
                args.movetimeMs = parseIntArg(argv[++i], "movetime-ms");
                break;
            case "--iterations":
            case "--runs":
                args.iterations = parseIntArg(argv[++i], "iterations");
                break;
            case "--warmup":
                args.warmup = parseIntArg(argv[++i], "warmup");
                break;
            case "--tt-size-mb":
                args.ttSizeMb = parseIntArg(argv[++i], "tt-size-mb");
                break;
            case "--multi-pv":
                args.multiPv = parseIntArg(argv[++i], "multi-pv");
                break;
            case "--help":
                console.log(usage());
                process.exit(0);
            default:
                throw new Error(`Unknown arg: ${arg}`);
        }
    }

    if (args.useNnue && !args.nnueFile) {
        throw new Error("--nnue-file is required unless --material is set");
    }

    return args;
};

const resolvePath = (value, fallback) => {
    if (!value) return fallback;
    return path.isAbsolute(value) ? value : path.resolve(process.cwd(), value);
};

const readBytes = (label, filePath) => {
    try {
        return readFileSync(filePath);
    } catch {
        throw new Error(`${label} not found: ${filePath}`);
    }
};

const parsePositionsFile = (filePath) => {
    const text = readFileSync(filePath, "utf-8");
    const lines = text.split(/\r?\n/);
    const positions = [];

    for (let idx = 0; idx < lines.length; idx += 1) {
        const raw = lines[idx].trim();
        if (!raw || raw.startsWith("#")) continue;

        const sep = raw.indexOf("|");
        if (sep >= 0) {
            positions.push({
                name: raw.slice(0, sep).trim() || `position_${idx + 1}`,
                sfen: raw.slice(sep + 1).trim(),
            });
        } else {
            positions.push({ name: `position_${idx + 1}`, sfen: raw });
        }
    }

    if (!positions.length) {
        throw new Error(`No positions found in file: ${filePath}`);
    }

    return positions;
};

const buildPositions = (args) => {
    if (args.sfensPath) {
        const pathResolved = resolvePath(args.sfensPath);
        return parsePositionsFile(pathResolved);
    }
    if (args.sfen || args.moves.length) {
        return [
            {
                name: "custom",
                sfen: args.sfen ?? "startpos",
                moves: args.moves.length ? args.moves : undefined,
            },
        ];
    }
    return DEFAULT_POSITIONS;
};

const buildLimits = (args) => {
    if (args.movetimeMs) {
        return { movetimeMs: args.movetimeMs };
    }
    return { nodes: args.nodes };
};

const collectSystemInfo = () => {
    const cpus = os.cpus();
    return {
        timestamp: new Date().toISOString(),
        cpu_model: cpus[0]?.model ?? "Unknown",
        cpu_cores: cpus.length || 1,
        os: os.type(),
        arch: process.arch,
    };
};

const toBenchResult = (position, elapsedMs, info, bestmove) => {
    const nodes = info?.nodes ?? 0;
    const timeMs = info?.timeMs ?? Math.round(elapsedMs);
    const nps = info?.nps ?? (timeMs > 0 && nodes > 0 ? Math.round((nodes / timeMs) * 1000) : 0);
    return {
        sfen: position.sfen,
        depth: info?.depth ?? 0,
        nodes,
        time_ms: timeMs,
        nps,
        hashfull: info?.hashfull ?? 0,
        bestmove: bestmove ?? "resign",
    };
};

const runBenchmark = async (args) => {
    const wasmPath = resolvePath(
        args.wasmPath,
        path.resolve(__dirname, "../pkg/engine_wasm_bg.wasm"),
    );
    const nnuePath = args.nnueFile ? resolvePath(args.nnueFile) : null;

    const wasmBytes = readBytes("wasm binary", wasmPath);

    await initWasm({ module_or_path: wasmBytes });
    initEngine({ ttSizeMb: args.ttSizeMb, multiPv: args.multiPv });
    if (args.useNnue) {
        const nnueBytes = readBytes("NNUE file", nnuePath);
        loadModel(nnueBytes);
    }

    let lastInfo = null;
    let lastBestmove = null;
    setEventHandler((event) => {
        if (!event || typeof event !== "object") return;
        if (event.type === "info") {
            lastInfo = event;
        } else if (event.type === "bestmove") {
            lastBestmove = event.move ?? null;
        }
    });

    const positions = buildPositions(args);
    const limits = buildLimits(args);

    const runOnce = (position) => {
        const moves = position.moves?.length ? position.moves : undefined;
        loadPosition(position.sfen, moves);
        lastInfo = null;
        lastBestmove = null;
        const start = performance.now();
        runSearch({ limits });
        const end = performance.now();
        return toBenchResult(position, end - start, lastInfo, lastBestmove);
    };

    for (let i = 0; i < args.warmup; i += 1) {
        for (const position of positions) {
            runOnce(position);
        }
    }

    const results = [];
    for (let i = 0; i < args.iterations; i += 1) {
        for (const position of positions) {
            results.push(runOnce(position));
        }
    }

    const report = {
        system_info: collectSystemInfo(),
        engine_name: "wasm",
        engine_path: wasmPath,
        eval_info: {
            nnue_enabled: args.useNnue,
            nnue_file: args.useNnue ? nnuePath : undefined,
            material_level: 9,
        },
        results: [{ threads: 1, results }],
    };

    console.log(JSON.stringify(report, null, 2));
};

runBenchmark(parseArgs(process.argv.slice(2))).catch((error) => {
    console.error(error.message);
    console.error("");
    console.error(usage());
    process.exitCode = 1;
});
