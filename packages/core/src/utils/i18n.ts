import { PIECE_NAMES, PROMOTED_PIECE_NAMES, type PieceType } from "../domain/model/piece";

// サポートする言語
export type SupportedLocale = "ja" | "en";

// 現在の言語設定（デフォルト：日本語）
let currentLocale: SupportedLocale = "ja";

// 言語設定を変更
export const setLocale = (locale: SupportedLocale): void => {
    currentLocale = locale;
};

// 現在の言語設定を取得
export const getLocale = (): SupportedLocale => currentLocale;

// 駒名の翻訳
export const translatePieceName = (
    pieceType: PieceType,
    promoted = false,
    locale?: SupportedLocale,
): string => {
    const targetLocale = locale ?? currentLocale;

    if (promoted && PROMOTED_PIECE_NAMES[targetLocale][pieceType]) {
        return PROMOTED_PIECE_NAMES[targetLocale][pieceType];
    }

    return PIECE_NAMES[targetLocale][pieceType];
};

// ゲーム用語の翻訳マップ
export const GAME_TERMS = {
    ja: {
        // 基本用語
        black: "先手",
        white: "後手",
        turn: "手番",
        move: "指し手",
        capture: "取得",
        drop: "打ち込み",
        promote: "成り",
        check: "王手",
        checkmate: "詰み",
        draw: "引き分け",
        resign: "投了",

        // ルール関連
        nifu: "二歩",
        uchifuzume: "打ち歩詰め",
        deadPiece: "行き所のない駒",
        sennichite: "千日手",
        perpetualCheck: "連続王手の千日手",

        // 盤面
        board: "盤面",
        hands: "持ち駒",
        square: "マス",
        rank: "段",
        file: "筋",

        // ゲーム結果
        win: "勝利",
        lose: "敗北",
        timeout: "時間切れ",
    },
    en: {
        // 基本用語
        black: "Black",
        white: "White",
        turn: "Turn",
        move: "Move",
        capture: "Capture",
        drop: "Drop",
        promote: "Promote",
        check: "Check",
        checkmate: "Checkmate",
        draw: "Draw",
        resign: "Resign",

        // ルール関連
        nifu: "Double Pawn",
        uchifuzume: "Pawn Drop Mate",
        deadPiece: "Immobile Piece",
        sennichite: "Repetition Draw",
        perpetualCheck: "Perpetual Check",

        // 盤面
        board: "Board",
        hands: "Pieces in Hand",
        square: "Square",
        rank: "Rank",
        file: "File",

        // ゲーム結果
        win: "Win",
        lose: "Lose",
        timeout: "Timeout",
    },
} as const;

// ゲーム用語の翻訳
export const translateGameTerm = (
    term: keyof typeof GAME_TERMS.ja,
    locale?: SupportedLocale,
): string => {
    const targetLocale = locale ?? currentLocale;
    return GAME_TERMS[targetLocale][term];
};

// エラーメッセージの翻訳
export const ERROR_MESSAGES = {
    ja: {
        invalidMove: "無効な手です",
        nifu: "二歩です",
        uchifuzume: "打ち歩詰めです",
        deadPiece: "行き所のない駒です",
        noPieceInHand: "その駒を持っていません",
        squareOccupied: "マスが空いていません",
        noPieceToMove: "移動元に駒がありません",
        notYourPiece: "自分の駒ではありません",
        cannotCaptureOwnPiece: "自駒へは移動できません",
    },
    en: {
        invalidMove: "Invalid move",
        nifu: "Double pawn",
        uchifuzume: "Pawn drop mate",
        deadPiece: "Immobile piece",
        noPieceInHand: "No such piece in hand",
        squareOccupied: "Square is occupied",
        noPieceToMove: "No piece to move",
        notYourPiece: "Not your piece",
        cannotCaptureOwnPiece: "Cannot capture own piece",
    },
} as const;

// エラーメッセージの翻訳
export const translateErrorMessage = (
    error: keyof typeof ERROR_MESSAGES.ja,
    locale?: SupportedLocale,
): string => {
    const targetLocale = locale ?? currentLocale;
    return ERROR_MESSAGES[targetLocale][error];
};

// 座標の表示形式変換（日本語：1一、英語：1a）
export const formatSquare = (row: number, column: number, locale?: SupportedLocale): string => {
    const targetLocale = locale ?? currentLocale;

    if (targetLocale === "ja") {
        const kanjiNumbers = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
        return `${column}${kanjiNumbers[row]}`;
    }
    const letters = ["", "a", "b", "c", "d", "e", "f", "g", "h", "i"];
    return `${column}${letters[row]}`;
};
