import type { Board } from "../domain/model/board";
import type { Move } from "../domain/model/move";
import type { Piece, PieceType } from "../domain/model/piece";
import type { Hands } from "../domain/service/moveService";

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
    addEntry(
        board: Board,
        hands: Hands,
        moves: OpeningMove[],
        depth: number,
        turn: "b" | "w" = "b",
    ): void {
        if (depth > this.maxDepth) return;

        const sfen = this.boardToKey(board, hands, turn);
        this.entries.set(sfen, {
            position: sfen,
            moves,
            depth,
        });
    }

    // 局面から定跡手を取得
    getMove(board: Board, hands: Hands, turn: "b" | "w" = "b"): Move | null {
        const sfen = this.boardToKey(board, hands, turn);
        const entry = this.entries.get(sfen);

        if (!entry || entry.moves.length === 0) {
            return null;
        }

        // 重み付けランダム選択
        return this.selectWeightedMove(entry.moves);
    }

    // 局面が定跡に含まれるかチェック
    hasPosition(board: Board, hands: Hands, turn: "b" | "w" = "b"): boolean {
        const sfen = this.boardToKey(board, hands, turn);
        return this.entries.has(sfen);
    }

    // 定跡の候補手をすべて取得
    getMoves(board: Board, hands: Hands, turn: "b" | "w" = "b"): OpeningMove[] {
        const sfen = this.boardToKey(board, hands, turn);
        const entry = this.entries.get(sfen);
        return entry ? entry.moves : [];
    }

    // 局面をキーに変換（盤面＋手番＋持ち駒のみ、手数は含めない）
    boardToKey(board: Board, hands: Hands, turn: "b" | "w" = "b"): string {
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

        // 手番
        sfen += ` ${turn} `;

        // 持ち駒
        let handsStr = "";
        const pieceOrder = ["rook", "bishop", "gold", "silver", "knight", "lance", "pawn"];

        // 先手の持ち駒
        for (const pieceType of pieceOrder) {
            const count = hands.black[pieceType as keyof typeof hands.black] || 0;
            if (count > 0) {
                if (count > 1) handsStr += count;
                handsStr += this.pieceToSFEN({
                    type: pieceType as PieceType,
                    owner: "black",
                    promoted: false,
                });
            }
        }

        // 後手の持ち駒
        for (const pieceType of pieceOrder) {
            const count = hands.white[pieceType as keyof typeof hands.white] || 0;
            if (count > 0) {
                if (count > 1) handsStr += count;
                handsStr += this.pieceToSFEN({
                    type: pieceType as PieceType,
                    owner: "white",
                    promoted: false,
                });
            }
        }

        sfen += handsStr || "-";

        // 手数は定跡の判定には不要なので含めない
        // 局面（盤面＋手番＋持ち駒）のみで判定

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

    // 定跡データを増分追加（メモリ効率化）
    addEntries(newEntries: OpeningEntry[]): void {
        for (const entry of newEntries) {
            this.entries.set(entry.position, entry);
        }
    }

    // エントリー数を取得
    size(): number {
        return this.entries.size;
    }

    // メモリ使用量の推定（バイト）
    estimateMemoryUsage(): number {
        // 各エントリーのサイズを推定
        // SFEN文字列: 約100バイト
        // Move配列: 平均3手 × 50バイト = 150バイト
        // その他のメタデータ: 50バイト
        // 合計: 約300バイト/エントリー
        return this.entries.size * 300;
    }

    // メモリ使用量をMB単位で取得
    getMemoryUsageMB(): number {
        return Math.round((this.estimateMemoryUsage() / 1024 / 1024) * 10) / 10;
    }

    // 深い定跡エントリーを削除（メモリ節約）
    removeDeepEntries(maxDepth: number): void {
        const entriesToRemove: string[] = [];

        for (const [key, entry] of this.entries) {
            if (entry.depth > maxDepth) {
                entriesToRemove.push(key);
            }
        }

        for (const key of entriesToRemove) {
            this.entries.delete(key);
        }

        console.log(
            `🗑️ ${entriesToRemove.length} 個の深い定跡エントリーを削除しました（深さ>${maxDepth}）`,
        );
    }

    // デバッグ用：すべてのエントリーを取得
    // 警告：大量のエントリーがある場合はメモリ不足の原因となる可能性があります
    // 使用する際は注意してください（80万エントリー以上ある場合があります）
    getAllEntries(): OpeningEntry[] {
        return Array.from(this.entries.values());
    }
}
