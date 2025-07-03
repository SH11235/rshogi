import type { Board, Column, Hands, Move, Piece, Row } from "shogi-core";
import { applyMove, initialHands, modernInitialBoard } from "shogi-core";
import type { OpeningEntry, OpeningMove } from "./openingBook";

// 型安全な行・列変換ヘルパー
function r(row: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9): Row {
    return row as Row;
}

function c(col: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9): Column {
    return col as Column;
}

// 定跡の手順を生成するジェネレータークラス
export class OpeningGenerator {
    private board: Board;
    private hands: Hands;
    private moveHistory: Move[];
    private entries: Map<string, OpeningEntry>;

    constructor() {
        this.board = modernInitialBoard;
        this.hands = initialHands();
        this.moveHistory = [];
        this.entries = new Map();
    }

    // 初期状態にリセット
    reset(): void {
        this.board = modernInitialBoard;
        this.hands = initialHands();
        this.moveHistory = [];
    }

    // 手を進める
    makeMove(move: Move): void {
        const result = applyMove(this.board, this.hands, this.getCurrentPlayer(), move);
        this.board = result.board;
        this.hands = result.hands;
        this.moveHistory.push(move);
    }

    // 現在の手番を取得
    getCurrentPlayer(): "black" | "white" {
        return this.moveHistory.length % 2 === 0 ? "black" : "white";
    }

    // 局面をSFEN形式に変換（簡易版）
    boardToSFEN(): string {
        let sfen = "";

        // 盤面の各段を変換
        for (let row = 1; row <= 9; row++) {
            let emptyCount = 0;
            for (let col = 9; col >= 1; col--) {
                const piece = this.board[`${row}${col}` as keyof Board];
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

    // 定跡エントリーを追加
    addOpeningEntry(moves: OpeningMove[]): void {
        const sfen = this.boardToSFEN();
        this.entries.set(sfen, {
            position: sfen,
            moves,
            depth: this.moveHistory.length,
        });
    }

    // すべてのエントリーを取得
    getAllEntries(): OpeningEntry[] {
        return Array.from(this.entries.values());
    }
}

// 主要な定跡データを生成
export function generateMainOpenings(): OpeningEntry[] {
    const generator = new OpeningGenerator();
    const entries: OpeningEntry[] = [];

    // 初期局面
    entries.push({
        position: generator.boardToSFEN(),
        moves: [
            {
                move: createPawnMove(r(7), c(7), r(6), c(7), "black"),
                weight: 40,
                name: "居飛車",
                comment: "最も一般的な初手",
            },
            {
                move: createPawnMove(r(7), c(2), r(6), c(2), "black"),
                weight: 30,
                name: "相掛かり",
                comment: "積極的な作戦",
            },
            {
                move: createPawnMove(r(7), c(5), r(6), c(5), "black"),
                weight: 20,
                name: "中飛車",
                comment: "振り飛車の一種",
            },
            {
                move: createPawnMove(r(7), c(9), r(6), c(9), "black"),
                weight: 10,
                name: "端歩",
                comment: "やや特殊な作戦",
            },
        ],
        depth: 0,
    });

    // 7六歩の後の手順
    generator.reset();
    generator.makeMove(createPawnMove(r(7), c(7), r(6), c(7), "black"));
    generator.makeMove(createPawnMove(r(3), c(4), r(4), c(4), "white"));

    entries.push({
        position: generator.boardToSFEN(),
        moves: [
            {
                move: createPawnMove(r(7), c(2), r(6), c(2), "black"),
                weight: 35,
                name: "矢倉",
                comment: "堅実な作戦",
            },
            {
                move: createPawnMove(r(7), c(6), r(6), c(6), "black"),
                weight: 30,
                name: "四間飛車",
                comment: "人気の振り飛車",
            },
            {
                move: createRookMove(r(8), c(2), r(6), c(2), "black"),
                weight: 20,
                name: "右四間飛車",
                comment: "攻撃的な作戦",
            },
            {
                move: createBishopMove(r(8), c(8), r(2), c(2), "black"),
                weight: 15,
                name: "角換わり",
                comment: "プロ好みの作戦",
            },
        ],
        depth: 2,
    });

    // 矢倉の手順
    generator.makeMove(createPawnMove(r(7), c(2), r(6), c(2), "black"));
    generator.makeMove(createPawnMove(r(3), c(5), r(4), c(5), "white"));

    entries.push({
        position: generator.boardToSFEN(),
        moves: [
            {
                move: createPawnMove(r(6), c(2), r(5), c(2), "black"),
                weight: 50,
                name: "矢倉",
                comment: "飛車先を伸ばす",
            },
            {
                move: createSilverMove(r(9), c(7), r(8), c(7), "black"),
                weight: 30,
                name: "矢倉",
                comment: "銀を活用",
            },
            {
                move: createGoldMove(r(9), c(6), r(8), c(7), "black"),
                weight: 20,
                name: "矢倉",
                comment: "金を上がる",
            },
        ],
        depth: 4,
    });

    return entries;
}

// ヘルパー関数：歩の手を作成
function createPawnMove(
    fromRow: Row,
    fromCol: Column,
    toRow: Row,
    toCol: Column,
    owner: "black" | "white",
): Move {
    return {
        type: "move",
        from: { row: fromRow, column: fromCol },
        to: { row: toRow, column: toCol },
        piece: { type: "pawn", owner, promoted: false },
        promote: false,
        captured: null,
    };
}

// ヘルパー関数：飛車の手を作成
function createRookMove(
    fromRow: Row,
    fromCol: Column,
    toRow: Row,
    toCol: Column,
    owner: "black" | "white",
): Move {
    return {
        type: "move",
        from: { row: fromRow, column: fromCol },
        to: { row: toRow, column: toCol },
        piece: { type: "rook", owner, promoted: false },
        promote: false,
        captured: null,
    };
}

// ヘルパー関数：角の手を作成
function createBishopMove(
    fromRow: Row,
    fromCol: Column,
    toRow: Row,
    toCol: Column,
    owner: "black" | "white",
): Move {
    return {
        type: "move",
        from: { row: fromRow, column: fromCol },
        to: { row: toRow, column: toCol },
        piece: { type: "bishop", owner, promoted: false },
        promote: false,
        captured: null,
    };
}

// ヘルパー関数：銀の手を作成
function createSilverMove(
    fromRow: Row,
    fromCol: Column,
    toRow: Row,
    toCol: Column,
    owner: "black" | "white",
): Move {
    return {
        type: "move",
        from: { row: fromRow, column: fromCol },
        to: { row: toRow, column: toCol },
        piece: { type: "silver", owner, promoted: false },
        promote: false,
        captured: null,
    };
}

// ヘルパー関数：金の手を作成
function createGoldMove(
    fromRow: Row,
    fromCol: Column,
    toRow: Row,
    toCol: Column,
    owner: "black" | "white",
): Move {
    return {
        type: "move",
        from: { row: fromRow, column: fromCol },
        to: { row: toRow, column: toCol },
        piece: { type: "gold", owner, promoted: false },
        promote: false,
        captured: null,
    };
}
