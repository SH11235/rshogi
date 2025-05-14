export type Player = "black" | "white";

// 駒の基本種類（日本語表記）
export type PieceKind = "歩" | "香" | "桂" | "銀" | "金" | "角" | "飛" | "王" | "玉";

// 持ち駒になる駒
export type HandKind = "歩" | "香" | "桂" | "銀" | "金" | "角" | "飛";

// 成れる駒
export const promotableKinds = ["歩", "香", "桂", "銀", "角", "飛"] as const;

// 成駒の表記（表示用途に便利）
export const promotedLabelMap = {
    歩: "と",
    香: "成香",
    桂: "成桂",
    銀: "成銀",
    金: "金",
    角: "馬",
    飛: "龍",
    王: "王",
    玉: "玉",
} as const;

// 駒型定義（成・不成、持ち主）
export type Piece = {
    kind: PieceKind;
    promoted: boolean;
    owner: Player;
};

const isPromotableKind = (k: PieceKind): k is PromotableKind =>
    (promotableKinds as readonly PieceKind[]).includes(k);

// 成れるかどうか
export const canPromote = (piece: Piece): boolean => {
    return !piece.promoted && isPromotableKind(piece.kind);
};

// 成駒化
export const promote = (piece: Piece): Piece =>
    canPromote(piece) ? { ...piece, promoted: true } : piece;

// ラベル表示用（駒名 or 成駒名）
export const getPieceLabel = (piece: Piece): string => {
    return piece.promoted ? promotedLabelMap[piece.kind] : piece.kind;
};

export type PromotableKind = (typeof promotableKinds)[number];
