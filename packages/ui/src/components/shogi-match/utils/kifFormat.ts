/**
 * KIF形式変換ユーティリティ
 *
 * USI形式の指し手を日本語KIF形式に変換する
 */

import type { BoardState, Piece, PieceType, Player, Square } from "@shogi/app-core";

/** KIF形式の指し手情報 */
export interface KifMove {
    /** 手数（1始まり） */
    ply: number;
    /** KIF形式の指し手文字列（例: "▲７六歩"） */
    kifText: string;
    /** USI形式の指し手（内部保持用） */
    usiMove: string;
    /** 評価値（センチポーン） */
    evalCp?: number;
    /** 詰み手数（正=勝ち、負=負け） */
    evalMate?: number;
    /** 探索深さ */
    depth?: number;
}

/** 評価値の履歴（グラフ用） */
export interface EvalHistory {
    ply: number;
    /** 評価値（センチポーン）。null = 詰み or 未計算 */
    evalCp: number | null;
    /** 詰み手数。null = 詰みなし */
    evalMate: number | null;
}

// ============================================================
// 漢数字テーブル
// ============================================================

/** 筋（ファイル）の漢数字：1〜9 */
const FILE_KANJI: readonly string[] = ["", "１", "２", "３", "４", "５", "６", "７", "８", "９"];

/** 段（ランク）の漢数字：1〜9 (a=1, b=2, ..., i=9) */
const RANK_KANJI: readonly string[] = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"];

// ============================================================
// 駒名テーブル
// ============================================================

/** 駒種 → 日本語名（通常） */
const PIECE_NAMES: Readonly<Record<PieceType, string>> = {
    P: "歩",
    L: "香",
    N: "桂",
    S: "銀",
    G: "金",
    B: "角",
    R: "飛",
    K: "玉",
};

/** 駒種 → 日本語名（成り駒） */
const PROMOTED_NAMES: Readonly<Record<PieceType, string>> = {
    P: "と",
    L: "成香",
    N: "成桂",
    S: "成銀",
    G: "金", // 金は成れないがフォールバック用
    B: "馬",
    R: "龍",
    K: "玉", // 玉は成れないがフォールバック用
};

// ============================================================
// 変換関数
// ============================================================

/**
 * USI形式のマス座標をKIF形式（漢数字）に変換
 * @param sq "5e" のようなUSI形式マス座標
 * @returns "５五" のような漢数字表記
 */
export function squareToKanji(sq: string): string {
    const file = parseInt(sq[0], 10); // 1-9
    const rankChar = sq[1]; // 'a'-'i'
    const rank = rankChar.charCodeAt(0) - 96; // a=1, b=2, ..., i=9

    if (file < 1 || file > 9 || rank < 1 || rank > 9) {
        return sq; // フォールバック
    }

    return `${FILE_KANJI[file]}${RANK_KANJI[rank]}`;
}

/**
 * USI形式の指し手から移動先マスを取得
 * @param usiMove "7g7f" or "P*5e" or "7g7f+"
 * @returns 移動先マス（例: "7f"）
 */
export function parseToSquare(usiMove: string): Square | undefined {
    if (!usiMove || usiMove.length < 4) return undefined;

    // 駒打ち: "P*5e"
    if (usiMove[1] === "*") {
        return usiMove.slice(2, 4) as Square;
    }

    // 通常移動: "7g7f" or "7g7f+"
    return usiMove.slice(2, 4) as Square;
}

/**
 * 駒種と成り状態から日本語駒名を取得
 */
export function getPieceName(pieceType: PieceType, promoted: boolean): string {
    if (promoted) {
        return PROMOTED_NAMES[pieceType] ?? PIECE_NAMES[pieceType];
    }
    return PIECE_NAMES[pieceType] ?? pieceType;
}

/**
 * USI形式の指し手をKIF形式に変換
 *
 * @param usiMove USI形式の指し手（例: "7g7f", "P*5e", "7g7f+"）
 * @param turn 手番
 * @param board 現在の盤面状態（指し手適用前）
 * @param prevTo 直前の移動先マス（「同」表記判定用）
 * @returns KIF形式の指し手文字列（例: "▲７六歩"）
 */
export function formatMoveToKif(
    usiMove: string,
    turn: Player,
    board: BoardState,
    prevTo?: Square,
): string {
    const mark = turn === "sente" ? "▲" : "△";

    if (!usiMove || usiMove.length < 4) {
        return `${mark}${usiMove}`; // フォールバック
    }

    // 駒打ち: "P*5e"
    if (usiMove[1] === "*") {
        const pieceChar = usiMove[0].toUpperCase() as PieceType;
        const to = usiMove.slice(2, 4);
        const toKanji = squareToKanji(to);
        const pieceName = PIECE_NAMES[pieceChar] ?? pieceChar;
        return `${mark}${toKanji}${pieceName}打`;
    }

    // 通常移動: "7g7f" or "7g7f+"
    const from = usiMove.slice(0, 2) as Square;
    const to = usiMove.slice(2, 4) as Square;
    const promotes = usiMove.endsWith("+");
    const piece: Piece | null = board[from];

    if (!piece) {
        // 盤面に駒がない場合（エラーケース）はUSI形式をそのまま返す
        return `${mark}${usiMove}`;
    }

    // 「同」表記判定：直前の移動先と今回の移動先が同じ場合
    const toKanji = prevTo === to ? "同　" : squareToKanji(to);

    // 駒名を取得（移動前の状態で判定）
    const pieceName = getPieceName(piece.type, piece.promoted ?? false);

    // 成り表記
    const promoteText = promotes ? "成" : "";

    // 不成表記：成れる状況で成らなかった場合に「不成」を付ける
    // （ここでは簡易実装として成り判定は省略）

    return `${mark}${toKanji}${pieceName}${promoteText}`;
}

/**
 * 評価値を表示用文字列にフォーマット
 * @param evalCp 評価値（センチポーン）
 * @param evalMate 詰み手数
 * @returns フォーマットされた文字列（例: "+50", "詰3", "被詰5"）
 */
export function formatEval(evalCp?: number, evalMate?: number): string {
    if (evalMate !== undefined && evalMate !== null) {
        if (evalMate > 0) {
            return `詰${evalMate}`;
        }
        return `被詰${Math.abs(evalMate)}`;
    }

    if (evalCp === undefined || evalCp === null) {
        return "";
    }

    // センチポーンを100で割って表示（小数点1桁）
    const value = evalCp / 100;
    if (value >= 0) {
        return `+${value.toFixed(1)}`;
    }
    return value.toFixed(1);
}

/**
 * 評価値をグラフ用Y座標に変換
 * @param evalCp 評価値（センチポーン）
 * @param evalMate 詰み手数
 * @param height グラフの高さ
 * @param clampValue 評価値のクランプ範囲（デフォルト: ±2000cp）
 * @returns Y座標（0 = 上端、height = 下端）
 */
export function evalToY(
    evalCp: number | null | undefined,
    evalMate: number | null | undefined,
    height: number,
    clampValue = 2000,
): number {
    const center = height / 2;

    // 詰みの場合は上端または下端に固定
    if (evalMate !== undefined && evalMate !== null) {
        return evalMate > 0 ? 4 : height - 4; // 少しマージンを取る
    }

    if (evalCp === undefined || evalCp === null) {
        return center; // 未計算は中央
    }

    // 評価値をクランプして正規化
    const clamped = Math.max(-clampValue, Math.min(clampValue, evalCp));
    // 正の値は上（Y小）、負の値は下（Y大）
    const normalized = -clamped / clampValue; // -1 ~ +1
    return center + normalized * (center - 4); // マージン考慮
}

/**
 * 複数の指し手をKIF形式に一括変換
 *
 * @param moves USI形式の指し手配列
 * @param boardHistory 各手直前の盤面状態の配列（moves と同じ長さ）
 * @param evalMap 手数 → 評価値イベントのマップ（オプション）
 * @returns KifMove の配列
 */
export function convertMovesToKif(
    moves: string[],
    boardHistory: BoardState[],
    evalMap?: Map<number, { scoreCp?: number; scoreMate?: number; depth?: number }>,
): KifMove[] {
    const kifMoves: KifMove[] = [];
    let prevTo: Square | undefined;

    for (let i = 0; i < moves.length; i++) {
        const ply = i + 1;
        const turn: Player = i % 2 === 0 ? "sente" : "gote";
        const board = boardHistory[i];
        const evalEvent = evalMap?.get(ply);

        if (!board) {
            // 盤面履歴がない場合はスキップ
            continue;
        }

        const kifText = formatMoveToKif(moves[i], turn, board, prevTo);

        kifMoves.push({
            ply,
            kifText,
            usiMove: moves[i],
            evalCp: evalEvent?.scoreCp,
            evalMate: evalEvent?.scoreMate,
            depth: evalEvent?.depth,
        });

        // 次の「同」判定用に移動先を記録
        prevTo = parseToSquare(moves[i]);
    }

    return kifMoves;
}

// ============================================================
// KIFファイル形式エクスポート
// ============================================================

/** KIFファイルエクスポート用のオプション */
interface KifExportOptions {
    /** 先手の名前 */
    senteName?: string;
    /** 後手の名前 */
    goteName?: string;
    /** 手合割（デフォルト: "平手"） */
    handicap?: string;
    /** 開始日時 */
    startTime?: Date;
    /** 終了日時 */
    endTime?: Date;
    /** 対局場所 */
    place?: string;
    /** 持ち時間（秒） */
    timeLimit?: number;
    /** 秒読み（秒） */
    byoyomi?: number;
}

/**
 * USI形式のマス座標を数字形式に変換（移動元表示用）
 * @param sq "7g" のようなUSI形式マス座標
 * @returns "77" のような数字表記
 */
function squareToDigits(sq: string): string {
    const file = sq[0]; // 1-9
    const rankChar = sq[1]; // 'a'-'i'
    const rank = rankChar.charCodeAt(0) - 96; // a=1, b=2, ..., i=9
    return `${file}${rank}`;
}

/**
 * 1手をKIFファイル形式でフォーマット
 *
 * @param ply 手数
 * @param usiMove USI形式の指し手
 * @param board 指し手適用前の盤面
 * @param prevTo 直前の移動先マス（「同」表記判定用）
 * @returns KIFファイル形式の1行（例: "   1 ２六歩(27)   ( 0:00/00:00:00)"）
 */
function formatMoveForKifFile(
    ply: number,
    usiMove: string,
    board: BoardState,
    prevTo?: Square,
): string {
    const plyStr = ply.toString().padStart(4, " ");

    if (!usiMove || usiMove.length < 4) {
        return `${plyStr} ${usiMove}`;
    }

    // 駒打ち: "P*5e"
    if (usiMove[1] === "*") {
        const pieceChar = usiMove[0].toUpperCase() as PieceType;
        const to = usiMove.slice(2, 4);
        const toKanji = squareToKanji(to);
        const pieceName = PIECE_NAMES[pieceChar] ?? pieceChar;
        // 駒打ちの場合は「打」を付ける
        return `${plyStr} ${toKanji}${pieceName}打     ( 0:00/00:00:00)`;
    }

    // 通常移動: "7g7f" or "7g7f+"
    const from = usiMove.slice(0, 2) as Square;
    const to = usiMove.slice(2, 4) as Square;
    const promotes = usiMove.endsWith("+");
    const piece: Piece | null = board[from];

    if (!piece) {
        return `${plyStr} ${usiMove}`;
    }

    // 「同」表記判定
    const toKanji = prevTo === to ? "同　" : squareToKanji(to);

    // 駒名を取得
    const pieceName = getPieceName(piece.type, piece.promoted ?? false);

    // 成り表記
    const promoteText = promotes ? "成" : "";

    // 移動元を数字で表示
    const fromDigits = squareToDigits(from);

    return `${plyStr} ${toKanji}${pieceName}${promoteText}(${fromDigits})   ( 0:00/00:00:00)`;
}

/**
 * 完全なKIF形式の文字列を生成（クリップボードコピー用）
 *
 * @param moves USI形式の指し手配列
 * @param boardHistory 各手直前の盤面状態の配列
 * @param options エクスポートオプション
 * @returns KIF形式の完全な文字列
 */
export function exportToKifString(
    moves: string[],
    boardHistory: BoardState[],
    options: KifExportOptions = {},
): string {
    const lines: string[] = [];

    // ヘッダー
    lines.push("#KIF version=2.0 encoding=UTF-8");

    // 開始日時
    if (options.startTime) {
        const dateStr = formatDateTime(options.startTime);
        lines.push(`開始日時：${dateStr}`);
    }

    // 終了日時
    if (options.endTime) {
        const dateStr = formatDateTime(options.endTime);
        lines.push(`終了日時：${dateStr}`);
    }

    // 手合割
    lines.push(`手合割：${options.handicap ?? "平手"}　　`);

    // 先手・後手
    lines.push(`先手：${options.senteName ?? ""}`);
    lines.push(`後手：${options.goteName ?? ""}`);

    // 対局場所
    if (options.place) {
        lines.push(`場所：${options.place}`);
    }

    // 持ち時間
    if (options.timeLimit !== undefined || options.byoyomi !== undefined) {
        const timeLimitStr = options.timeLimit ? `${Math.floor(options.timeLimit / 60)}分` : "0分";
        const byoyomiStr = options.byoyomi ? `${options.byoyomi}秒` : "0秒";
        lines.push(`持ち時間：${timeLimitStr}+${byoyomiStr}`);
    }

    // 指し手ヘッダー
    lines.push("手数----指手---------消費時間--");

    // 各手
    let prevTo: Square | undefined;
    const validMoves = moves.slice(0, boardHistory.length);

    for (let i = 0; i < validMoves.length; i++) {
        const ply = i + 1;
        const board = boardHistory[i];

        if (!board) continue;

        const moveLine = formatMoveForKifFile(ply, validMoves[i], board, prevTo);
        lines.push(moveLine);

        prevTo = parseToSquare(validMoves[i]);
    }

    // 終局表示（手数がある場合）
    if (validMoves.length > 0) {
        lines.push("");
    }

    return lines.join("\n");
}

/**
 * 日時をKIF形式でフォーマット
 */
function formatDateTime(date: Date): string {
    const y = date.getFullYear();
    const m = (date.getMonth() + 1).toString().padStart(2, "0");
    const d = date.getDate().toString().padStart(2, "0");
    const h = date.getHours().toString().padStart(2, "0");
    const min = date.getMinutes().toString().padStart(2, "0");
    const s = date.getSeconds().toString().padStart(2, "0");
    return `${y}/${m}/${d} ${h}:${min}:${s}`;
}
