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

// 駒型定義（成・不成、持ち主）
// 新形式：type プロパティ使用（推奨）
export type ModernPiece = {
    type: PieceType;
    promoted: boolean;
    owner: Player;
};

// レガシー形式：kind プロパティ使用（後方互換性）
export type LegacyPiece = {
    kind: string; // 日本語駒名
    promoted: boolean;
    owner: Player;
};

// 段階的移行のためのユニオン型
export type Piece = ModernPiece | LegacyPiece;

// 型ガード：新形式かどうかを判定
export const isModernPiece = (piece: Piece): piece is ModernPiece => {
    return "type" in piece;
};

// 型ガード：レガシー形式かどうかを判定
export const isLegacyPiece = (piece: Piece): piece is LegacyPiece => {
    return "kind" in piece;
};

// 型ガード：成駒可能かチェック
const isPromotablePieceType = (type: PieceType): type is PromotablePieceType =>
    (PROMOTABLE_PIECE_TYPES as readonly PieceType[]).includes(type);

// 駒のタイプを統一的に取得（内部用）
const getPieceTypeFromPiece = (piece: Piece): PieceType => {
    if (isModernPiece(piece)) {
        return piece.type;
    }
    return convertLegacyPieceType(piece.kind);
};

// 成れるかどうか
export const canPromote = (piece: Piece): boolean => {
    const pieceType = getPieceTypeFromPiece(piece);
    return !piece.promoted && isPromotablePieceType(pieceType);
};

// 成駒化（入力と同じ形式で返す）
export const promote = (piece: Piece): Piece => {
    if (!canPromote(piece)) {
        return piece;
    }

    return { ...piece, promoted: true };
};

// 駒名取得（指定言語）
export const getPieceName = (piece: Piece, locale: "ja" | "en" = "ja"): string => {
    const pieceType = getPieceTypeFromPiece(piece);

    if (piece.promoted && isPromotablePieceType(pieceType)) {
        return PROMOTED_PIECE_NAMES[locale][pieceType];
    }
    return PIECE_NAMES[locale][pieceType];
};

// 日本語から新形式への変換ヘルパー（移行期間用）
export const convertLegacyPieceType = (legacyKind: string): PieceType => {
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

    return conversionMap[legacyKind] || "pawn";
};

// 新形式から日本語への変換ヘルパー（移行期間用）
export const convertToLegacyPieceKind = (pieceType: PieceType): string => {
    return PIECE_NAMES.ja[pieceType];
};

// 駒を新形式に変換
export const toModernPiece = (piece: Piece): ModernPiece => {
    if (isModernPiece(piece)) {
        return piece;
    }
    return {
        type: convertLegacyPieceType(piece.kind),
        promoted: piece.promoted,
        owner: piece.owner,
    };
};

// 駒をレガシー形式に変換
export const toLegacyPiece = (piece: Piece): LegacyPiece => {
    if (isLegacyPiece(piece)) {
        return piece;
    }
    return {
        kind: PIECE_NAMES.ja[piece.type],
        promoted: piece.promoted,
        owner: piece.owner,
    };
};

// 新形式の駒を作成するヘルパー
export const createModernPiece = (
    type: PieceType,
    owner: Player,
    promoted = false,
): ModernPiece => ({
    type,
    promoted,
    owner,
});

// レガシー形式の駒を作成するヘルパー（テスト用など）
export const createLegacyPiece = (kind: string, owner: Player, promoted = false): LegacyPiece => ({
    kind,
    promoted,
    owner,
});

// 駒のタイプをチェックするユーティリティ関数
export const isPieceOfType = (piece: Piece, pieceType: PieceType): boolean => {
    return getPieceTypeFromPiece(piece) === pieceType;
};

// 特定の駒タイプをチェックするヘルパー関数群
export const isPawn = (piece: Piece): boolean => isPieceOfType(piece, "pawn");
export const isLance = (piece: Piece): boolean => isPieceOfType(piece, "lance");
export const isKnight = (piece: Piece): boolean => isPieceOfType(piece, "knight");
export const isSilver = (piece: Piece): boolean => isPieceOfType(piece, "silver");
export const isGold = (piece: Piece): boolean => isPieceOfType(piece, "gold");
export const isBishop = (piece: Piece): boolean => isPieceOfType(piece, "bishop");
export const isRook = (piece: Piece): boolean => isPieceOfType(piece, "rook");
export const isKing = (piece: Piece): boolean => isPieceOfType(piece, "king");
export const isGyoku = (piece: Piece): boolean => isPieceOfType(piece, "gyoku");

// 王または玉かをチェック（両方をカバー）
export const isRoyalPiece = (piece: Piece): boolean => isKing(piece) || isGyoku(piece);

// レガシー形式のチェック（日本語）- 段階的移行用
export const isLegacyPieceOfKind = (piece: Piece, kind: string): boolean => {
    if (isLegacyPiece(piece)) {
        return piece.kind === kind;
    }
    return PIECE_NAMES.ja[piece.type] === kind;
};
