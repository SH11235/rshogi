export type Player = "black" | "white";

// 駒の基本種類（英語キー、国際化対応）
export type PieceType =
    | "pawn"
    | "lance"
    | "knight"
    | "silver"
    | "gold"
    | "bishop"
    | "rook"
    | "king"
    | "gyoku";

// 持ち駒になる駒（王・玉は除外）
export type HandPieceType = "pawn" | "lance" | "knight" | "silver" | "gold" | "bishop" | "rook";

// 成れる駒
export const PROMOTABLE_PIECE_TYPES = [
    "pawn",
    "lance",
    "knight",
    "silver",
    "bishop",
    "rook",
] as const;
export type PromotablePieceType = (typeof PROMOTABLE_PIECE_TYPES)[number];

// 国際化対応の駒名マップ
export const PIECE_NAMES = {
    ja: {
        pawn: "歩",
        lance: "香",
        knight: "桂",
        silver: "銀",
        gold: "金",
        bishop: "角",
        rook: "飛",
        king: "王",
        gyoku: "玉",
    },
    en: {
        pawn: "Pawn",
        lance: "Lance",
        knight: "Knight",
        silver: "Silver",
        gold: "Gold",
        bishop: "Bishop",
        rook: "Rook",
        king: "King",
        gyoku: "Jeweled General",
    },
} as const;

// 成駒の表記（国際化対応）
export const PROMOTED_PIECE_NAMES = {
    ja: {
        pawn: "と",
        lance: "成香",
        knight: "成桂",
        silver: "成銀",
        gold: "金", // 金は成駒なし
        bishop: "馬",
        rook: "龍",
        king: "王", // 王は成駒なし
        gyoku: "玉", // 玉は成駒なし
    },
    en: {
        pawn: "Tokin",
        lance: "Promoted Lance",
        knight: "Promoted Knight",
        silver: "Promoted Silver",
        gold: "Gold", // 金は成駒なし
        bishop: "Horse",
        rook: "Dragon",
        king: "King", // 王は成駒なし
        gyoku: "Jeweled General", // 玉は成駒なし
    },
} as const;

// 駒型定義（ModernPieceに統一）
export type Piece = {
    type: PieceType;
    promoted: boolean;
    owner: Player;
};

// 型ガード：成駒可能かチェック
const isPromotablePieceType = (type: PieceType): type is PromotablePieceType =>
    (PROMOTABLE_PIECE_TYPES as readonly PieceType[]).includes(type);

// 成れるかどうか
export const canPromote = (piece: Piece): boolean => {
    return !piece.promoted && isPromotablePieceType(piece.type);
};

// 成駒化
export const promote = (piece: Piece): Piece => {
    if (!canPromote(piece)) {
        return piece;
    }
    return { ...piece, promoted: true };
};

// 駒名取得（指定言語）
export const getPieceName = (piece: Piece, locale: "ja" | "en" = "ja"): string => {
    if (piece.promoted && isPromotablePieceType(piece.type)) {
        return PROMOTED_PIECE_NAMES[locale][piece.type];
    }
    return PIECE_NAMES[locale][piece.type];
};

// 駒を作成するヘルパー（createModernPiece を createPiece にリネーム）
export const createPiece = (type: PieceType, owner: Player, promoted = false): Piece => ({
    type,
    promoted,
    owner,
});

// 特定の駒タイプをチェックするヘルパー関数群
export const isPawn = (piece: Piece): boolean => piece.type === "pawn";
export const isLance = (piece: Piece): boolean => piece.type === "lance";
export const isKnight = (piece: Piece): boolean => piece.type === "knight";
export const isSilver = (piece: Piece): boolean => piece.type === "silver";
export const isGold = (piece: Piece): boolean => piece.type === "gold";
export const isBishop = (piece: Piece): boolean => piece.type === "bishop";
export const isRook = (piece: Piece): boolean => piece.type === "rook";
export const isKing = (piece: Piece): boolean => piece.type === "king";
export const isGyoku = (piece: Piece): boolean => piece.type === "gyoku";

// 王または玉かをチェック（両方をカバー）
export const isRoyalPiece = (piece: Piece): boolean => isKing(piece) || isGyoku(piece);

// 日本語駒名から PieceType への変換（初期盤面などで使用）
export const convertJapanesePieceType = (japaneseName: string): PieceType => {
    const conversionMap: Record<string, PieceType> = {
        歩: "pawn",
        香: "lance",
        桂: "knight",
        銀: "silver",
        金: "gold",
        角: "bishop",
        飛: "rook",
        王: "king",
        玉: "gyoku",
    };
    return conversionMap[japaneseName] || "pawn";
};

// PieceType から日本語駒名への変換
export const convertToJapaneseName = (pieceType: PieceType): string => {
    return PIECE_NAMES.ja[pieceType];
};

// 後方互換性のためのエイリアス（段階的に削除予定）
export const createModernPiece = createPiece;
export type ModernPiece = Piece;
