import { Board, getPiece } from "../model/board";
import { Square, Row, Column } from "../model/square";
import { Piece, Player } from "../model/piece";

export type Move = {
    from: Square;
    to: Square;
    promote?: boolean;
};

const directions: Record<string, [number, number][]> = {
    歩: [[-1, 0]],
    香: Array.from({ length: 8 }, (_, i) => [-1 - i, 0]),
    桂: [
        [-2, -1],
        [-2, 1],
    ],
    銀: [
        [-1, -1],
        [-1, 0],
        [-1, 1],
        [1, -1],
        [1, 1],
    ],
    金: [
        [-1, -1],
        [-1, 0],
        [-1, 1],
        [0, -1],
        [0, 1],
        [1, 0],
    ],
    成銀: [
        [-1, -1],
        [-1, 0],
        [-1, 1],
        [0, -1],
        [0, 1],
        [1, 0],
    ],
    成桂: [
        [-1, -1],
        [-1, 0],
        [-1, 1],
        [0, -1],
        [0, 1],
        [1, 0],
    ],
    成香: [
        [-1, -1],
        [-1, 0],
        [-1, 1],
        [0, -1],
        [0, 1],
        [1, 0],
    ],
    と: [
        [-1, -1],
        [-1, 0],
        [-1, 1],
        [0, -1],
        [0, 1],
        [1, 0],
    ],
    角: Array.from({ length: 8 }, (_, i) => i + 1).flatMap((n) => [
        [-n, -n],
        [-n, n],
        [n, -n],
        [n, n],
    ]),
    馬: [
        [-1, 0],
        [1, 0],
        [0, -1],
        [0, 1],
    ].concat(
        Array.from({ length: 8 }, (_, i) => i + 1).flatMap((n) => [
            [-n, -n],
            [-n, n],
            [n, -n],
            [n, n],
        ]),
    ),
    飛: Array.from({ length: 8 }, (_, i) => i + 1).flatMap((n) => [
        [-n, 0],
        [n, 0],
        [0, -n],
        [0, n],
    ]),
    龍: [
        [-1, -1],
        [-1, 1],
        [1, -1],
        [1, 1],
    ].concat(
        Array.from({ length: 8 }, (_, i) => i + 1).flatMap((n) => [
            [-n, 0],
            [n, 0],
            [0, -n],
            [0, n],
        ]),
    ),
    王: [
        [-1, -1],
        [-1, 0],
        [-1, 1],
        [0, -1],
        [0, 1],
        [1, -1],
        [1, 0],
        [1, 1],
    ],
};

const inBounds = (row: number, col: number): row is Row & number =>
    row >= 1 && row <= 9 && col >= 1 && col <= 9;

export const generateMoves = (board: Board, square: Square): Move[] => {
    const piece = getPiece(board, square);
    if (!piece) return [];

    const label = piece.promoted
        ? {
              歩: "と",
              香: "成香",
              桂: "成桂",
              銀: "成銀",
              角: "馬",
              飛: "龍",
              金: "金",
              王: "王",
          }[piece.kind]
        : piece.kind;

    const dir = directions[label];
    if (!dir) return [];

    const forward = piece.owner === "black" ? -1 : 1;
    const moves: Move[] = [];

    for (const [dr, dc] of dir) {
        let r = square.row;
        let c = square.column;

        while (true) {
            r += piece.owner === "black" ? dr : -dr;
            c += piece.owner === "black" ? dc : -dc;

            if (!inBounds(r, c)) break;

            const target: Square = { row: r as Row, column: c as Column };
            const dest = getPiece(board, target);

            if (dest?.owner === piece.owner) break;

            const move: Move = { from: square, to: target };

            // 成り選択を仮で追加（実運用ではエリア判定が必要）
            if (canPromoteArea(piece, square, target)) {
                moves.push({ ...move, promote: true });
            }

            moves.push(move);

            if (dest) break; // 相手の駒を取ったら止まる
            if (!isLongRange(label)) break; // 短距離駒は1マスだけ
        }
    }

    return moves;
};

const isLongRange = (label: string): boolean => ["香", "角", "飛", "龍", "馬"].includes(label);

// 仮：3段目ルールを無視して、成れる駒は全部成れるようにする（後で修正）
const canPromoteArea = (piece: Piece, from: Square, to: Square): boolean => {
    return !piece.promoted && ["歩", "香", "桂", "銀", "角", "飛"].includes(piece.kind);
};
