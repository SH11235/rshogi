import type { OpeningEntry } from "./openingBook";
import type { Move } from "../domain/model/move";

/**
 * 基本的な定跡データを生成
 * フォールバック用の最小限の定跡
 */
export function generateMainOpenings(): OpeningEntry[] {
    const entries: OpeningEntry[] = [];

    // 初期局面
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -",
        depth: 20,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 7 },
                    to: { row: 7, column: 6 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "7g7f",
                weight: 100,
                name: "居飛車",
                comment: "最も一般的な初手",
            },
            {
                move: {
                    type: "move",
                    from: { row: 2, column: 7 },
                    to: { row: 2, column: 6 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "2g2f",
                weight: 80,
                name: "振り飛車",
                comment: "飛車を振る準備",
            },
            {
                move: {
                    type: "move",
                    from: { row: 6, column: 7 },
                    to: { row: 6, column: 6 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "6g6f",
                weight: 60,
                name: "相振り飛車",
                comment: "相振り飛車の可能性",
            },
            {
                move: {
                    type: "move",
                    from: { row: 5, column: 7 },
                    to: { row: 5, column: 6 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "5g5f",
                weight: 40,
                name: "中飛車",
                comment: "中飛車戦法",
            },
        ],
    });

    // 7六歩後の局面
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w -",
        depth: 18,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 3 },
                    to: { row: 3, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "3c3d",
                weight: 90,
                name: "角道を開ける",
            },
            {
                move: {
                    type: "move",
                    from: { row: 8, column: 3 },
                    to: { row: 8, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "8c8d",
                weight: 70,
                name: "横歩取り対策",
            },
            {
                move: {
                    type: "move",
                    from: { row: 4, column: 3 },
                    to: { row: 4, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "4c4d",
                weight: 50,
                name: "中央志向",
            },
        ],
    });

    // 矢倉の基本形への道
    entries.push({
        position: "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b -",
        depth: 18,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 6, column: 7 },
                    to: { row: 6, column: 6 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "6g6f",
                weight: 100,
                name: "矢倉",
                comment: "矢倉囲いへの第一歩",
            },
            {
                move: {
                    type: "move",
                    from: { row: 2, column: 7 },
                    to: { row: 2, column: 6 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "2g2f",
                weight: 60,
                name: "転換",
            },
        ],
    });

    // 四間飛車の基本形への道（2六歩後）
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL w -",
        depth: 18,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 3 },
                    to: { row: 3, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "3c3d",
                weight: 80,
            },
            {
                move: {
                    type: "move",
                    from: { row: 4, column: 3 },
                    to: { row: 4, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "4c4d",
                weight: 70,
                name: "四間飛車対策",
            },
        ],
    });

    // 2八飛後の四間飛車
    entries.push({
        position: "lnsgkgsnl/1r5b1/pppp1pppp/4p4/9/7P1/PPPPPPP1P/1B2R4/LNSGKGSNL b -",
        depth: 16,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 2, column: 8 },
                    to: { row: 6, column: 8 },
                    piece: { type: "rook", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "2h6h",
                weight: 100,
                name: "四間飛車",
                comment: "四間飛車の完成",
            },
            {
                move: {
                    type: "move",
                    from: { row: 7, column: 7 },
                    to: { row: 7, column: 6 },
                    piece: { type: "pawn", owner: "black", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "7g7f",
                weight: 60,
            },
        ],
    });

    // 相掛かりの基本形
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/7R1/LNSGKGSNL w 2L",
        depth: 16,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 8, column: 3 },
                    to: { row: 8, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "8c8d",
                weight: 100,
                name: "相掛かり",
                comment: "相掛かり戦法の進行",
            },
        ],
    });

    // 角換わりの基本形
    entries.push({
        position: "lnsgkgsnl/1r7/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w b",
        depth: 16,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 8, column: 2 },
                    to: { row: 2, column: 2 },
                    piece: { type: "bishop", owner: "white", promoted: false },
                    promote: true,
                    captured: null,
                } as Move,
                notation: "8b2b+",
                weight: 90,
                name: "角換わり",
            },
        ],
    });

    // 中飛車の基本形
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/4P4/PPPP1PPPP/1B5R1/LNSGKGSNL w -",
        depth: 16,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 5, column: 3 },
                    to: { row: 5, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "5c5d",
                weight: 90,
                name: "中飛車対策",
            },
        ],
    });

    // ゴキゲン中飛車
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w -",
        depth: 16,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 5, column: 3 },
                    to: { row: 5, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "5c5d",
                weight: 80,
                name: "ゴキゲン中飛車",
                comment: "角道を止めない中飛車",
            },
            {
                move: {
                    type: "move",
                    from: { row: 8, column: 3 },
                    to: { row: 8, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "8c8d",
                weight: 70,
            },
        ],
    });

    // 石田流三間飛車への道
    entries.push({
        position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P4P1/PP1PPPP1P/1B5R1/LNSGKGSNL w -",
        depth: 15,
        moves: [
            {
                move: {
                    type: "move",
                    from: { row: 4, column: 3 },
                    to: { row: 4, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "4c4d",
                weight: 80,
            },
            {
                move: {
                    type: "move",
                    from: { row: 3, column: 3 },
                    to: { row: 3, column: 4 },
                    piece: { type: "pawn", owner: "white", promoted: false },
                    promote: false,
                    captured: null,
                } as Move,
                notation: "3c3d",
                weight: 70,
                name: "石田流対策",
            },
        ],
    });

    return entries;
}
