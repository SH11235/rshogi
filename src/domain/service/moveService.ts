// -------------------------------------------------------
// 盤面・持ち駒など **ドメインモデル** を純粋関数で操作するサービス層。
// React／Zustand／I/O には一切依存しない。
// -------------------------------------------------------

import { type Board, getPiece, setPiece } from "../model/board";
import type { Move, NormalMove } from "../model/move";
import { type HandKind, type Piece, type Player, promote } from "../model/piece";
import type { Column, Row, Square } from "../model/square";

//--------------------------------------------------------
// 持ち駒ヘルパー
//--------------------------------------------------------

/**
 * hands.black["歩"] === 2 なら「先手が歩を２枚所持」を意味する。
 * 王は持ち駒にならないため HandKind をキーに採用。
 */
export type Hands = {
    black: Record<HandKind, number>;
    white: Record<HandKind, number>;
};

export const createEmptyHands = (): Hands => ({
    black: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
    white: { 歩: 0, 香: 0, 桂: 0, 銀: 0, 金: 0, 角: 0, 飛: 0 },
});

/** 手番を反転するユーティリティ */
export const toggleSide = (p: Player): Player => (p === "black" ? "white" : "black");

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
    // Board は浅い構造なのでスプレッドで OK
    let newBoard: Board = { ...board };
    // Hands はネストしているので深いコピーを取る
    const newHands: Hands = JSON.parse(JSON.stringify(hands));

    const mover = currentTurn;
    const nextTurn = toggleSide(currentTurn);

    if (move.type === "move") {
        /* ---------- 通常の指し手 ---------- */
        const srcPiece = getPiece(board, move.from);
        if (!srcPiece) throw new Error("移動元に駒がありません");
        if (srcPiece.owner !== mover) throw new Error("自分の駒ではありません");

        const dstPiece = getPiece(board, move.to);
        if (dstPiece && dstPiece.owner === mover) throw new Error("自駒へは移動不可");

        // 取った駒を持ち駒へ
        if (dstPiece) newHands[mover][dstPiece.kind as HandKind] += 1;

        // 移動
        newBoard = setPiece(newBoard, move.from, null);
        const placed: Piece = move.promote ? promote(srcPiece) : { ...srcPiece };
        newBoard = setPiece(newBoard, move.to, placed);
    } else if (move.type === "drop") {
        /* ---------- 打ち手（持ち駒を置く） ---------- */
        const kind = move.piece.kind as HandKind;
        if (newHands[mover][kind] <= 0) throw new Error("その駒を持っていません");
        if (getPiece(board, move.to)) throw new Error("マスが空いていません");

        newHands[mover][kind] -= 1;
        newBoard = setPiece(newBoard, move.to, { ...move.piece, owner: mover, promoted: false });
    } else {
        /* ---------- それ以外（未定義の Move タイプ） ---------- */
        assertNever(move);
    }

    return { board: newBoard, hands: newHands, nextTurn };
}

/**
 * exhaustive check 用ユーティリティ。
 * 引数が never 型でなければコンパイルエラーになる。
 */
function assertNever(_value: never): never {
    throw new Error("未定義の Move タイプ");
}

/** Undo 用 – 1 手戻して盤・持ち駒を復元 */
export function revertMove(
    board: Board,
    hands: Hands,
    currentTurn: Player, // ★適用後の手番（＝相手側）
    move: Move,
): ApplyMoveResult {
    const mover = toggleSide(currentTurn); // 元の指し手側
    let newBoard: Board = { ...board };
    const newHands: Hands = JSON.parse(JSON.stringify(hands));

    if (move.type === "move") {
        const dstPiece = getPiece(board, move.to);
        if (!dstPiece) throw new Error("Undo 先に駒がありません");

        const original = move.promote ? { ...dstPiece, promoted: false } : dstPiece;

        // 取った駒を戻す / 駒を元位置へ
        newBoard = setPiece(newBoard, move.to, move.captured);
        newBoard = setPiece(newBoard, move.from, original);

        if (move.captured) newHands[mover][move.captured.kind as HandKind] -= 1;
    } else {
        // 打ち手を戻す
        newBoard = setPiece(newBoard, move.to, null);
        newHands[mover][move.piece.kind as HandKind] += 1;
    }

    return { board: newBoard, hands: newHands, nextTurn: mover };
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

    switch (piece.kind) {
        case "歩":
            piece.promoted ? v.push(...gold) : add(f, 0);
            break;
        case "香":
            piece.promoted ? v.push(...gold) : add(f, 0, true);
            break;
        case "桂":
            if (piece.promoted) {
                // 成桂（成り金）は金と同じ動き
                v.push(...gold);
            } else {
                // 未成桂は前方斜め２マスジャンプ
                add(2 * f, -1);
                add(2 * f, 1);
            }
            break;
        case "銀":
            piece.promoted ? v.push(...gold) : v.push(...silver);
            break;
        case "金":
            v.push(...gold);
            break;
        case "角":
            v.push([1, 1, true], [1, -1, true], [-1, 1, true], [-1, -1, true]);
            if (piece.promoted) v.push(...king.filter(([dr, dc]) => dr === 0 || dc === 0));
            break;
        case "飛":
            v.push([1, 0, true], [-1, 0, true], [0, 1, true], [0, -1, true]);
            if (piece.promoted)
                v.push(...king.filter(([dr, dc]) => Math.abs(dr) === 1 && Math.abs(dc) === 1));
            break;
        case "王":
            v.push(...king);
            break;
    }
    return v;
}
