import type { Board, Hands, Move, Piece } from "shogi-core";

// 定跡エントリーの型定義
export interface OpeningMove {
    move: Move;
    weight: number; // 選択される確率の重み（1-100）
    name?: string; // 定跡名（例：「矢倉」「四間飛車」）
    comment?: string; // この手についてのコメント
}

export interface OpeningEntry {
    position: string; // SFEN形式の局面
    moves: OpeningMove[]; // この局面での候補手
    depth: number; // 初手からの手数
}

// 定跡データベースクラス
export class OpeningBook {
    private entries: Map<string, OpeningEntry>;
    private maxDepth: number;

    constructor(maxDepth = 20) {
        this.entries = new Map();
        this.maxDepth = maxDepth;
    }

    // 定跡エントリーを追加
    addEntry(board: Board, hands: Hands, moves: OpeningMove[], depth: number): void {
        if (depth > this.maxDepth) return;

        const sfen = this.boardToKey(board, hands);
        this.entries.set(sfen, {
            position: sfen,
            moves,
            depth,
        });
    }

    // 局面から定跡手を取得
    getMove(board: Board, hands: Hands, depth: number): Move | null {
        if (depth > this.maxDepth) return null;

        const sfen = this.boardToKey(board, hands);
        const entry = this.entries.get(sfen);

        if (!entry || entry.moves.length === 0) {
            return null;
        }

        // 重み付けランダム選択
        return this.selectWeightedMove(entry.moves);
    }

    // 局面が定跡に含まれるかチェック
    hasPosition(board: Board, hands: Hands): boolean {
        const sfen = this.boardToKey(board, hands);
        return this.entries.has(sfen);
    }

    // 定跡の候補手をすべて取得
    getMoves(board: Board, hands: Hands): OpeningMove[] {
        const sfen = this.boardToKey(board, hands);
        const entry = this.entries.get(sfen);
        return entry ? entry.moves : [];
    }

    // 局面をキーに変換（SFEN形式の簡略版）
    private boardToKey(board: Board, _hands: Hands): string {
        // boardをSFEN形式に変換
        let sfen = "";

        // 盤面の各段を変換
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

    // 駒をSFEN文字に変換
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

    // 重み付けランダム選択
    private selectWeightedMove(moves: OpeningMove[]): Move {
        const totalWeight = moves.reduce((sum, m) => sum + m.weight, 0);
        let random = Math.random() * totalWeight;

        for (const moveEntry of moves) {
            random -= moveEntry.weight;
            if (random <= 0) {
                return moveEntry.move;
            }
        }

        // フォールバック（通常は到達しない）
        return moves[0].move;
    }

    // 定跡データをロード
    loadFromData(data: OpeningEntry[]): void {
        this.entries.clear();
        for (const entry of data) {
            this.entries.set(entry.position, entry);
        }
    }

    // エントリー数を取得
    size(): number {
        return this.entries.size;
    }

    // デバッグ用：すべてのエントリーを取得
    getAllEntries(): OpeningEntry[] {
        return Array.from(this.entries.values());
    }
}
