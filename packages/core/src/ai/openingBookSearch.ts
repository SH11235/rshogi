import type { Board } from "../domain/model/board";
import type { Piece } from "../domain/model/piece";
import type { Hands } from "../domain/service/moveService";
import type { OpeningEntry, OpeningMove } from "./openingBook";

/**
 * 効率的な定跡検索のためのインデックス構造
 */
export interface OpeningIndex {
    // 局面ハッシュから候補手への高速アクセス
    positionMap: Map<string, OpeningEntry>;

    // 手順から局面への逆引きインデックス
    moveSequenceIndex: Map<string, string[]>; // moveSequence -> position keys

    // 深さごとの局面グループ
    depthIndex: Map<number, string[]>; // depth -> position keys

    // 定跡名から局面への索引
    openingNameIndex: Map<string, string[]>; // opening name -> position keys
}

// 局面ハッシュ計算用の定数
const PIECE_HASHES: Record<string, number> = {
    pawn_black: 0x12345678,
    pawn_white: 0x87654321,
    lance_black: 0x23456789,
    lance_white: 0x98765432,
    knight_black: 0x34567890,
    knight_white: 0x09876543,
    silver_black: 0x45678901,
    silver_white: 0x10987654,
    gold_black: 0x56789012,
    gold_white: 0x21098765,
    bishop_black: 0x67890123,
    bishop_white: 0x32109876,
    rook_black: 0x78901234,
    rook_white: 0x43210987,
    king_black: 0x89012345,
    king_white: 0x54321098,
    // 成駒用
    promoted_pawn_black: 0x90123456,
    promoted_pawn_white: 0x65432109,
    // ... 他の成駒も同様
};

/**
 * 局面の高速ハッシュ値を計算（定跡検索用）
 */
export function hashOpeningPosition(board: Board, hands: Hands): string {
    let hash = 0;

    // 盤面の駒をハッシュ化
    for (const [square, piece] of Object.entries(board)) {
        if (piece) {
            const pieceKey = `${piece.promoted ? "promoted_" : ""}${piece.type}_${piece.owner}`;
            const squareNum = Number.parseInt(square);
            hash ^= (PIECE_HASHES[pieceKey] || 0) * squareNum;
        }
    }

    // 持ち駒をハッシュ化
    for (const player of ["black", "white"] as const) {
        for (const [pieceType, count] of Object.entries(hands[player])) {
            if (count > 0) {
                hash ^= (PIECE_HASHES[`${pieceType}_${player}`] || 0) * count * 100;
            }
        }
    }

    return hash.toString(36); // 36進数で短縮
}

/**
 * 効率的な定跡検索アルゴリズム
 */
export class EfficientOpeningSearch {
    private index: OpeningIndex;
    private cache: Map<string, OpeningMove[]>; // LRUキャッシュ
    private cacheMaxSize = 1000;

    constructor() {
        this.index = {
            positionMap: new Map(),
            moveSequenceIndex: new Map(),
            depthIndex: new Map(),
            openingNameIndex: new Map(),
        };
        this.cache = new Map();
    }

    /**
     * 定跡データベースを効率的にインデックス化
     */
    buildIndex(entries: OpeningEntry[]): void {
        console.time("Building opening book index");

        this.index.positionMap.clear();
        this.index.moveSequenceIndex.clear();
        this.index.depthIndex.clear();
        this.index.openingNameIndex.clear();

        for (const entry of entries) {
            // 基本インデックス
            this.index.positionMap.set(entry.position, entry);

            // 深さインデックス
            const depthEntries = this.index.depthIndex.get(entry.depth) || [];
            depthEntries.push(entry.position);
            this.index.depthIndex.set(entry.depth, depthEntries);

            // 定跡名インデックス
            for (const move of entry.moves) {
                if (move.name) {
                    const nameEntries = this.index.openingNameIndex.get(move.name) || [];
                    nameEntries.push(entry.position);
                    this.index.openingNameIndex.set(move.name, nameEntries);
                }
            }
        }

        console.timeEnd("Building opening book index");
        console.log(`Indexed ${entries.length} positions`);
    }

    /**
     * 局面から候補手を高速検索
     */
    searchMoves(
        board: Board,
        hands: Hands,
        options?: {
            maxDepth?: number;
            openingName?: string;
            minWeight?: number;
        },
    ): OpeningMove[] {
        const positionKey = hashOpeningPosition(board, hands);

        // キャッシュチェック
        const cached = this.cache.get(positionKey);
        if (cached) {
            this.updateCache(positionKey, cached); // LRU更新
            return cached;
        }

        // SFEN形式での検索（フォールバック）
        const sfenKey = this.boardToSFEN(board, hands);
        const entry = this.index.positionMap.get(sfenKey);

        if (!entry) {
            return [];
        }

        // フィルタリング
        let moves = entry.moves;

        if (options?.maxDepth !== undefined && entry.depth > options.maxDepth) {
            return [];
        }

        if (options?.openingName) {
            moves = moves.filter((m) => m.name === options.openingName);
        }

        if (options?.minWeight !== undefined) {
            const minWeight = options.minWeight;
            moves = moves.filter((m) => m.weight >= minWeight);
        }

        // キャッシュに保存
        this.updateCache(positionKey, moves);

        return moves;
    }

    /**
     * 特定の定跡系統の手順を検索
     */
    searchByOpeningName(openingName: string, maxDepth?: number): Map<string, OpeningEntry> {
        const positions = this.index.openingNameIndex.get(openingName) || [];
        const result = new Map<string, OpeningEntry>();

        for (const posKey of positions) {
            const entry = this.index.positionMap.get(posKey);
            if (entry && (maxDepth === undefined || entry.depth <= maxDepth)) {
                result.set(posKey, entry);
            }
        }

        return result;
    }

    /**
     * 現在の局面から到達可能な定跡の深さを予測
     */
    estimateRemainingDepth(_board: Board, _hands: Hands, currentDepth: number): number {
        const deeperPositions = [];

        // 現在より深い局面を探索
        for (let depth = currentDepth + 1; depth <= currentDepth + 10; depth++) {
            const positions = this.index.depthIndex.get(depth);
            if (positions && positions.length > 0) {
                deeperPositions.push(depth);
            }
        }

        return deeperPositions.length > 0 ? Math.max(...deeperPositions) - currentDepth : 0;
    }

    /**
     * LRUキャッシュの更新
     */
    private updateCache(key: string, value: OpeningMove[]): void {
        // 既存のエントリを削除（LRU順序更新のため）
        this.cache.delete(key);

        // キャッシュサイズ制限
        if (this.cache.size >= this.cacheMaxSize) {
            const firstKey = this.cache.keys().next().value;
            if (firstKey !== undefined) {
                this.cache.delete(firstKey);
            }
        }

        this.cache.set(key, value);
    }

    /**
     * SFEN形式への変換（既存実装の簡易版）
     */
    private boardToSFEN(board: Board, _hands: Hands): string {
        let sfen = "";

        for (let row = 1; row <= 9; row++) {
            let emptyCount = 0;
            for (let col = 9; col >= 1; col--) {
                const piece = board[`${row}${col}` as keyof Board];
                if (piece) {
                    if (emptyCount > 0) {
                        sfen += emptyCount;
                        emptyCount = 0;
                    }
                    sfen += this.pieceToSFEN(piece);
                } else {
                    emptyCount++;
                }
            }
            if (emptyCount > 0) {
                sfen += emptyCount;
            }
            if (row < 9) sfen += "/";
        }

        return sfen;
    }

    private pieceToSFEN(piece: Piece): string {
        const pieceMap: Record<string, string> = {
            pawn: "p",
            lance: "l",
            knight: "n",
            silver: "s",
            gold: "g",
            bishop: "b",
            rook: "r",
            king: "k",
            gyoku: "k",
        };

        let char = pieceMap[piece.type] || "?";
        if (piece.promoted) char = `+${char}`;
        if (piece.owner === "black") char = char.toUpperCase();

        return char;
    }

    /**
     * メモリ使用量の統計情報
     */
    getStatistics(): {
        totalPositions: number;
        indexedByDepth: Map<number, number>;
        indexedByOpening: Map<string, number>;
        cacheSize: number;
        estimatedMemoryMB: number;
    } {
        const stats = {
            totalPositions: this.index.positionMap.size,
            indexedByDepth: new Map<number, number>(),
            indexedByOpening: new Map<string, number>(),
            cacheSize: this.cache.size,
            estimatedMemoryMB: 0,
        };

        // 深さ別統計
        for (const [depth, positions] of this.index.depthIndex) {
            stats.indexedByDepth.set(depth, positions.length);
        }

        // 定跡名別統計
        for (const [name, positions] of this.index.openingNameIndex) {
            stats.indexedByOpening.set(name, positions.length);
        }

        // メモリ推定（簡易版）
        const avgEntrySize = 400; // バイト（テスト結果から）
        stats.estimatedMemoryMB = (stats.totalPositions * avgEntrySize) / 1024 / 1024;

        return stats;
    }
}
