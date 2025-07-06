// -------------------------------------------------------
// 盤面・持ち駒など **ドメインモデル** を純粋関数で操作するサービス層。
// React／Zustand／I/O には一切依存しない。
// -------------------------------------------------------

import { type Board, getPiece, setPiece } from "../model/board";
import type { Move, NormalMove } from "../model/move";
import {
    type Piece,
    type Player,
    convertToJapaneseName,
    createPiece,
    isKnight,
    isLance,
    isPawn,
    isRoyalPiece,
    promote,
} from "../model/piece";
import type { Column, Row, Square } from "../model/square";
import { isCheckmate } from "./checkmate";
import { cloneBoardAndHands } from "./utils";

//--------------------------------------------------------
// 持ち駒ヘルパー
//--------------------------------------------------------

/**
 * hands.black["歩"] === 2 なら「先手が歩を２枚所持」を意味する。
 * 王は持ち駒にならないため文字列をキーに採用。
 */
export type Hands = {
    black: Record<string, number>;
    white: Record<string, number>;
};

export const initialHands = (): Hands => ({
    black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
    white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
});

// エイリアス（テストで使用）
export const createEmptyHands = initialHands;

/** 手番を反転するユーティリティ */
export const toggleSide = (p: Player): Player => (p === "black" ? "white" : "black");

/**
 * 二歩チェック: 指定した筋に同じプレイヤーの歩があるかチェック
 */
export function hasNifuViolation(board: Board, column: Column, player: Player): boolean {
    for (let row = 1; row <= 9; row++) {
        const piece = getPiece(board, { row: row as Row, column });
        if (piece && piece.owner === player && isPawn(piece) && !piece.promoted) {
            return true;
        }
    }
    return false;
}

/**
 * 行き所のない駒チェック: 指定位置に駒を置いた場合、その駒が動けなくなるかチェック
 */
export function isDeadPiece(piece: Piece, toRow: Row): boolean {
    // 先手は上向き（row数値が小さい方向）、後手は下向き（row数値が大きい方向）
    const isBlack = piece.owner === "black";

    if (isPawn(piece) || isLance(piece)) {
        // 歩と香は最奥段（先手なら1段目、後手なら9段目）で動けなくなる
        return isBlack ? toRow === 1 : toRow === 9;
    }

    if (isKnight(piece)) {
        // 桂は最奥2段（先手なら1,2段目、後手なら8,9段目）で動けなくなる
        return isBlack ? toRow <= 2 : toRow >= 8;
    }

    return false;
}

/**
 * 打ち歩詰めチェック: 歩を打って相手が詰みになるかチェック
 */
export function isUchifuzume(board: Board, dropSquare: Square, player: Player): boolean {
    // 仮に歩を打った盤面を作成
    const testBoard = setPiece(board, dropSquare, createPiece("pawn", player, false));

    // 相手が詰みかチェック
    const opponent = toggleSide(player);

    // 相手の王がいるか確認
    let hasOpponentKing = false;
    for (let row = 1; row <= 9; row++) {
        for (let col = 1; col <= 9; col++) {
            const piece = getPiece(testBoard, { row: row as Row, column: col as Column });
            if (piece && isRoyalPiece(piece) && piece.owner === opponent) {
                hasOpponentKing = true;
                break;
            }
        }
    }

    // 相手の王がいない場合は打ち歩詰めではない
    if (!hasOpponentKing) {
        return false;
    }

    return isCheckmate(testBoard, initialHands(), opponent);
}

//--------------------------------------------------------
// 1️⃣ 1 手を適用する applyMove / 戻す revertMove
//--------------------------------------------------------

export interface ApplyMoveResult {
    board: Board;
    hands: Hands;
    nextTurn: Player;
}

/**
 * 盤・持ち駒を **イミュータブル** に更新して返す。
 */
export function applyMove(
    board: Board,
    hands: Hands,
    currentTurn: Player,
    move: Move,
): ApplyMoveResult {
    const { board: newBoard, hands: newHands } = cloneBoardAndHands(board, hands);
    let updatedBoard = newBoard;

    const mover = currentTurn;
    const nextTurn = toggleSide(currentTurn);

    if (move.type === "move") {
        /* ---------- 通常の指し手 ---------- */
        const srcPiece = getPiece(board, move.from);
        if (!srcPiece) throw new Error("移動元に駒がありません");
        if (srcPiece.owner !== mover) throw new Error("自分の駒ではありません");

        const dstPiece = getPiece(board, move.to);
        if (dstPiece && dstPiece.owner === mover) throw new Error("自駒へは移動不可");

        // 取った駒を持ち駒へ（日本語名をキーとして使用）
        if (dstPiece) {
            const japaneseName = convertToJapaneseName(dstPiece.type);
            newHands[mover][japaneseName] += 1;
        }

        // 移動
        updatedBoard = setPiece(updatedBoard, move.from, null);

        // 成りの強制判定: 行き所のない位置に移動する場合は強制的に成る
        let mustPromote = false;
        if (!srcPiece.promoted && isDeadPiece(srcPiece, move.to.row)) {
            mustPromote = true;
        }

        const placed: Piece = move.promote || mustPromote ? promote(srcPiece) : { ...srcPiece };
        updatedBoard = setPiece(updatedBoard, move.to, placed);
    } else if (move.type === "drop") {
        /* ---------- 打ち手（持ち駒を置く） ---------- */
        const japaneseName = convertToJapaneseName(move.piece.type);
        if (newHands[mover][japaneseName] <= 0) throw new Error("その駒を持っていません");
        if (getPiece(board, move.to)) throw new Error("マスが空いていません");

        // 二歩チェック
        if (japaneseName === "歩" && hasNifuViolation(board, move.to.column, mover)) {
            throw new Error("二歩です");
        }

        // 行き所のない駒チェック
        if (isDeadPiece(move.piece, move.to.row)) {
            throw new Error("行き所のない駒です");
        }

        // 打ち歩詰めチェック
        if (japaneseName === "歩" && isUchifuzume(board, move.to, mover)) {
            throw new Error("打ち歩詰めです");
        }

        newHands[mover][japaneseName] -= 1;
        updatedBoard = setPiece(updatedBoard, move.to, {
            ...move.piece,
            owner: mover,
            promoted: false,
        });
    } else {
        /* ---------- それ以外（未定義の Move タイプ） ---------- */
        assertNever(move);
    }

    return { board: updatedBoard, hands: newHands, nextTurn };
}

/**
 * exhaustive check 用ユーティリティ。
 * 引数が never 型でなければコンパイルエラーになる。
 */
function assertNever(value: never): never {
    throw new Error(`未定義の Move タイプ: ${value}`);
}

/** Undo 用 – 1 手戻して盤・持ち駒を復元 */
export function revertMove(
    board: Board,
    hands: Hands,
    currentTurn: Player, // ★適用後の手番（＝相手側）
    move: Move,
): ApplyMoveResult {
    const mover = toggleSide(currentTurn); // 元の指し手側
    const { board: newBoard, hands: newHands } = cloneBoardAndHands(board, hands);
    let updatedBoard = newBoard;

    if (move.type === "move") {
        const dstPiece = getPiece(board, move.to);
        if (!dstPiece) throw new Error("Undo 先に駒がありません");

        const original = move.promote ? { ...dstPiece, promoted: false } : dstPiece;

        // 取った駒を戻す / 駒を元位置へ
        updatedBoard = setPiece(updatedBoard, move.to, move.captured);
        updatedBoard = setPiece(updatedBoard, move.from, original);

        if (move.captured) {
            const capturedJapaneseName = convertToJapaneseName(move.captured.type);
            newHands[mover][capturedJapaneseName] -= 1;
        }
    } else {
        // 打ち手を戻す
        updatedBoard = setPiece(updatedBoard, move.to, null);
        const pieceJapaneseName = convertToJapaneseName(move.piece.type);
        newHands[mover][pieceJapaneseName] += 1;
    }

    return { board: updatedBoard, hands: newHands, nextTurn: mover };
}

//--------------------------------------------------------
// 2️⃣ 履歴再実行 replayMoves
//--------------------------------------------------------

type Snapshot = { board: Board; hands: Hands; turn: Player };

export function replayMoves(initial: Snapshot, moves: Move[]): Snapshot {
    return moves.reduce((st, mv) => {
        const { board, hands, nextTurn } = applyMove(st.board, st.hands, st.turn, mv);
        return { board, hands, turn: nextTurn };
    }, initial);
}

//--------------------------------------------------------
// 3️⃣ 疑似合法手生成 generateMoves
//--------------------------------------------------------

/**
 * 自玉チェックは無視した“擬合法手”を列挙。
 * isInCheck の攻撃判定などに利用出来る。
 */
export function generateMoves(board: Board, from: Square): NormalMove[] {
    const piece = getPiece(board, from);
    if (!piece) return [];

    const moves: NormalMove[] = [];
    for (const [dr, dc, slide] of getMoveVectors(piece)) {
        let r = from.row as number;
        let c = from.column as number;
        while (true) {
            r += dr;
            c += dc;
            if (r < 1 || r > 9 || c < 1 || c > 9) break; // 盤外

            const sq: Square = { row: r as Row, column: c as Column };
            const target = getPiece(board, sq);
            if (target && target.owner === piece.owner) break; // 味方駒でブロック

            moves.push({
                type: "move",
                from,
                to: sq,
                piece,
                promote: false, // 成り判定は UI/ルール側で別途処理
                captured: target ?? null,
            });

            if (target || !slide) break; // 捕獲 or 非スライダー
        }
    }
    return moves;
}

//--------------------------------------------------------
// 4️⃣ 駒ごとの移動ベクトル定義
//--------------------------------------------------------

type Vec = [number, number, boolean]; // [dRow, dCol, スライド可能]

function getMoveVectors(piece: Piece): Vec[] {
    // 黒は上向き(-1), 白は下向き(+1)
    const f = piece.owner === "black" ? -1 : 1;
    const v: Vec[] = [];
    const add = (dr: number, dc: number, slide = false) => v.push([dr, dc, slide]);

    /* 金銀ステップなど共通定義 */
    const gold: Vec[] = [
        [f, 0, false],
        [f, -1, false],
        [f, 1, false],
        [0, -1, false],
        [0, 1, false],
        [-f, 0, false],
    ];
    const silver: Vec[] = [
        [f, 0, false],
        [f, -1, false],
        [f, 1, false],
        [-f, -1, false],
        [-f, 1, false],
    ];
    const king: Vec[] = [
        [1, 0, false],
        [-1, 0, false],
        [0, 1, false],
        [0, -1, false],
        [1, 1, false],
        [1, -1, false],
        [-1, 1, false],
        [-1, -1, false],
    ];

    switch (piece.type) {
        case "pawn":
            if (piece.promoted) {
                v.push(...gold);
            } else {
                add(f, 0);
            }
            break;
        case "lance":
            if (piece.promoted) {
                v.push(...gold);
            } else {
                add(f, 0, true);
            }
            break;
        case "knight":
            if (piece.promoted) {
                // 成桂（成り金）は金と同じ動き
                v.push(...gold);
            } else {
                // 未成桂は前方斜め２マスジャンプ
                add(2 * f, -1);
                add(2 * f, 1);
            }
            break;
        case "silver":
            if (piece.promoted) {
                v.push(...gold);
            } else {
                v.push(...silver);
            }
            break;
        case "gold":
            v.push(...gold);
            break;
        case "bishop":
            v.push([1, 1, true], [1, -1, true], [-1, 1, true], [-1, -1, true]);
            if (piece.promoted) v.push(...king.filter(([dr, dc]) => dr === 0 || dc === 0));
            break;
        case "rook":
            v.push([1, 0, true], [-1, 0, true], [0, 1, true], [0, -1, true]);
            if (piece.promoted)
                v.push(...king.filter(([dr, dc]) => Math.abs(dr) === 1 && Math.abs(dc) === 1));
            break;
        case "king":
        case "gyoku":
            v.push(...king);
            break;
    }
    return v;
}
