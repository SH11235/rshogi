import type { Board } from "../model/board";
import type { Player } from "../model/piece";
import type { MateSearchResult } from "./mateSearch";
import type { Hands } from "./moveService";

// WASM モジュールの型定義
interface WasmMateSearcher {
    search_from_json(
        boardJson: string,
        handsJson: string,
        attacker: string,
        maxDepth: number,
    ): {
        is_mate: boolean;
        node_count: number;
    };
    free?: () => void;
}

interface WasmModule {
    default?: () => Promise<void>;
    init?: () => Promise<void>;
    MateSearcher: new (timeout: number) => WasmMateSearcher;
}

// WASM モジュールの動的インポート
let wasmModule: WasmModule | null = null;
let isInitialized = false;

/**
 * WASMモジュールの初期化
 */
async function initializeWasm() {
    if (!isInitialized) {
        try {
            // 動的インポートを使用
            const importedModule = await import("../../../../web/src/wasm/shogi_core.js");
            wasmModule = importedModule as unknown as WasmModule;

            // WASMモジュールを初期化
            if (wasmModule.default) {
                await wasmModule.default();
            } else if (wasmModule.init) {
                await wasmModule.init();
            }

            isInitialized = true;
            console.log("WASM module initialized successfully");
        } catch (error) {
            console.error("Failed to initialize WASM module:", error);
            throw error;
        }
    }
}

/**
 * 盤面データをWASM用のJSON形式に変換
 */
function boardToJson(board: Board): string {
    const boardArray: Array<{
        square: string;
        piece: { type: string; owner: string; promoted: boolean };
    }> = [];
    for (const [key, piece] of Object.entries(board)) {
        if (piece) {
            boardArray.push({
                square: key,
                piece: {
                    type: piece.type,
                    owner: piece.owner,
                    promoted: piece.promoted,
                },
            });
        }
    }
    return JSON.stringify(boardArray);
}

/**
 * 持ち駒データをWASM用のJSON形式に変換
 */
function handsToJson(hands: Hands): string {
    return JSON.stringify({
        black: hands.black,
        white: hands.white,
    });
}

/**
 * WASM実装の詰み探索サービス
 */
export class MateSearchWasmService {
    private searcher: WasmMateSearcher | null = null;

    /**
     * WASMベースの詰み探索を実行
     */
    public async search(
        board: Board,
        hands: Hands,
        attacker: Player,
        options: { maxDepth: number; timeout?: number },
    ): Promise<MateSearchResult> {
        // WASMモジュールを初期化
        await initializeWasm();

        // 探索エンジンのインスタンスを作成
        if (!this.searcher && wasmModule) {
            this.searcher = new wasmModule.MateSearcher(
                options.timeout || 30000,
            ) as WasmMateSearcher;
        }

        // 盤面と持ち駒をJSON形式に変換
        const boardJson = boardToJson(board);
        const handsJson = handsToJson(hands);

        // searcherが作成されていることを確認
        if (!this.searcher) {
            throw new Error("Failed to create WASM mate searcher");
        }

        // WASM側で探索を実行
        const startTime = performance.now();
        const wasmResult = this.searcher.search_from_json(
            boardJson,
            handsJson,
            attacker,
            options.maxDepth,
        );

        // 結果を変換して返す
        return {
            isMate: wasmResult.is_mate,
            moves: [], // TODO: WASM側から手順を取得する実装
            nodeCount: wasmResult.node_count,
            elapsedMs: Math.round(performance.now() - startTime),
        };
    }

    /**
     * リソースのクリーンアップ
     */
    public dispose() {
        if (this.searcher) {
            // WASMリソースの解放
            this.searcher.free?.();
            this.searcher = null;
        }
    }
}

/**
 * WASMモジュールの事前ロード
 */
export async function preloadWasmModule(): Promise<void> {
    await initializeWasm();
}
