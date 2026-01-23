/**
 * KIF形式変換ユーティリティ
 *
 * USI形式の指し手を日本語KIF形式に変換する
 */

import type { BoardState, Piece, PieceType, Player, PositionState, Square } from "@shogi/app-core";
import { applyMoveWithState } from "@shogi/app-core";
import { parseSfen } from "./kifParser";

/** 単一PVの評価値情報 */
export interface PvEvalInfo {
    /** PV番号（1-indexed） */
    multipv: number;
    /** 評価値（センチポーン） */
    evalCp?: number;
    /** 詰み手数 */
    evalMate?: number;
    /** 探索深さ */
    depth?: number;
    /** 読み筋（USI形式） */
    pv?: string[];
}

/** KIF形式の指し手情報 */
export interface KifMove {
    /** 手数（1始まり） */
    ply: number;
    /** KIF形式の指し手文字列（例: "▲７六歩(77)"）- エクスポート用 */
    kifText: string;
    /** 簡易表示用文字列（例: "☗7六歩(77)"）- UI表示用 */
    displayText: string;
    /** USI形式の指し手（内部保持用） */
    usiMove: string;
    /** 評価値（センチポーン） */
    evalCp?: number;
    /** 詰み手数（正=勝ち、負=負け） */
    evalMate?: number;
    /** 探索深さ */
    depth?: number;
    /** 消費時間（ミリ秒） */
    elapsedMs?: number;
    /** 読み筋（USI形式の指し手配列） */
    pv?: string[];
    /** 複数PV用の評価値配列 */
    multiPvEvals?: PvEvalInfo[];
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

/** 同一マスへの移動を表す表記（全角スペースを含む） */
const SAME_SQUARE_TEXT = "同　";

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

const HIRATE_SFEN = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

const normalizeStartSfen = (sfen?: string): string | null => {
    if (!sfen) return null;
    const trimmed = sfen.trim();
    if (!trimmed) return null;
    const parsed = parseSfen(trimmed);
    return parsed.sfen || null;
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
 * @param usiMove "7g7f" or "P*5e" or "7g7f+" or "pass"
 * @returns 移動先マス（例: "7f"）、パス手の場合は undefined
 */
export function parseToSquare(usiMove: string): Square | undefined {
    if (!usiMove || usiMove.length < 4) return undefined;

    // パス手の場合は移動先マスがない
    if (usiMove.toLowerCase() === "pass") return undefined;

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
 * USI形式のマス座標を数字形式に変換（移動元表示用）
 * @param sq "7g" のようなUSI形式マス座標
 * @returns "77" のような数字表記
 */
function squareToDigits(sq: string): string {
    if (!sq || sq.length < 2) {
        return sq ?? ""; // フォールバック
    }
    const file = sq[0]; // 1-9
    const rankChar = sq[1]; // 'a'-'i'
    const rank = rankChar.charCodeAt(0) - 96; // a=1, b=2, ..., i=9
    return `${file}${rank}`;
}

/**
 * USI形式の指し手をKIF形式に変換
 *
 * @param usiMove USI形式の指し手（例: "7g7f", "P*5e", "7g7f+", "pass"）
 * @param turn 手番
 * @param board 現在の盤面状態（指し手適用前）
 * @param prevTo 直前の移動先マス（「同」表記判定用）
 * @returns KIF形式の指し手文字列（例: "▲７六歩(77)"）
 */
export function formatMoveToKif(
    usiMove: string,
    turn: Player,
    board: BoardState,
    prevTo?: Square,
): string {
    const mark = turn === "sente" ? "▲" : "△";

    // パス手の処理
    if (usiMove?.toLowerCase() === "pass") {
        return `${mark}パス`;
    }

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
    const toKanji = prevTo === to ? SAME_SQUARE_TEXT : squareToKanji(to);

    // 駒名を取得（移動前の状態で判定）
    const pieceName = getPieceName(piece.type, piece.promoted ?? false);

    // 成り表記
    const promoteText = promotes ? "成" : "";

    // 移動元座標
    const fromDigits = squareToDigits(from);

    return `${mark}${toKanji}${pieceName}${promoteText}(${fromDigits})`;
}

/**
 * USI形式の指し手を簡易表示形式に変換（UI表示用）
 *
 * 正式KIF形式との違い:
 * - 先手後手: ▲△ → ☗☖（Unicode駒記号）
 * - 筋: 全角（７）→ 半角（7）
 *
 * @param usiMove USI形式の指し手（例: "7g7f", "P*5e", "7g7f+", "pass"）
 * @param turn 手番
 * @param board 現在の盤面状態（指し手適用前）
 * @param prevTo 直前の移動先マス（「同」表記判定用）
 * @returns 簡易表示形式の指し手文字列（例: "☗7六歩(77)"）
 */
export function formatMoveSimple(
    usiMove: string,
    turn: Player,
    board: BoardState,
    prevTo?: Square,
): string {
    const mark = turn === "sente" ? "☗" : "☖";

    // パス手の処理
    if (usiMove?.toLowerCase() === "pass") {
        return `${mark}パス`;
    }

    if (!usiMove || usiMove.length < 4) {
        return `${mark}${usiMove}`; // フォールバック
    }

    // 駒打ち: "P*5e"
    if (usiMove[1] === "*") {
        const pieceChar = usiMove[0].toUpperCase() as PieceType;
        const to = usiMove.slice(2, 4);
        const toSimple = squareToSimple(to);
        const pieceName = PIECE_NAMES[pieceChar] ?? pieceChar;
        return `${mark}${toSimple}${pieceName}打`;
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
    const toSimple = prevTo === to ? SAME_SQUARE_TEXT : squareToSimple(to);

    // 駒名を取得（移動前の状態で判定）
    const pieceName = getPieceName(piece.type, piece.promoted ?? false);

    // 成り表記
    const promoteText = promotes ? "成" : "";

    // 移動元座標
    const fromDigits = squareToDigits(from);

    return `${mark}${toSimple}${pieceName}${promoteText}(${fromDigits})`;
}

/**
 * USI形式のマス座標を簡易表示形式に変換
 * @param sq "5e" のようなUSI形式マス座標
 * @returns "5五" のような半角数字+漢数字表記
 */
function squareToSimple(sq: string): string {
    if (!sq || sq.length < 2) {
        return sq ?? ""; // フォールバック
    }
    const file = sq[0]; // 半角数字のまま
    const rankChar = sq[1]; // 'a'-'i'
    const rank = rankChar.charCodeAt(0) - 96; // a=1, b=2, ..., i=9

    if (rank < 1 || rank > 9) {
        return sq; // フォールバック
    }

    return `${file}${RANK_KANJI[rank]}`;
}

/**
 * 評価値を表示用文字列にフォーマット
 *
 * 評価値は先手視点に正規化されていることを前提とする:
 * - evalMate > 0: 先手の勝ち（先手が詰ませる）
 * - evalMate < 0: 後手の勝ち（後手が詰ませる）
 * - evalCp > 0: 先手有利
 * - evalCp < 0: 後手有利
 *
 * @param evalCp 評価値（センチポーン、先手視点）
 * @param evalMate 詰み手数（先手視点）
 * @param _ply 手数（後方互換性のため残すが使用しない）
 * @returns フォーマットされた文字列（例: "+5.0", "+詰3", "-詰5"）
 */
export function formatEval(evalCp?: number, evalMate?: number, _ply?: number): string {
    if (evalMate !== undefined && evalMate !== null) {
        // 先手視点に正規化済みなので、符号だけで判定
        // 符号式: +詰N（先手勝ち）、-詰N（後手勝ち）
        if (evalMate > 0) {
            return `+詰${evalMate}`;
        }
        return `-詰${Math.abs(evalMate)}`;
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
 * 評価値のツールチップ用の詳細情報を生成
 *
 * 評価値は先手視点に正規化されていることを前提とする:
 * - evalMate > 0: 先手の勝ち
 * - evalMate < 0: 後手の勝ち
 * - evalCp > 0: 先手有利
 * - evalCp < 0: 後手有利
 *
 * @param evalCp 評価値（センチポーン、先手視点）
 * @param evalMate 詰み手数（先手視点）
 * @param _ply 手数（後方互換性のため残すが使用しない）
 * @param depth 探索深さ
 * @returns ツールチップ用の情報オブジェクト
 */
export function getEvalTooltipInfo(
    evalCp?: number,
    evalMate?: number,
    _ply?: number,
    depth?: number,
): {
    /** メイン説明（例: "☗先手有利"） */
    description: string;
    /** 詳細値（例: "+150cp"） */
    detail: string;
    /** 探索深さ（例: "深さ20"） */
    depthText: string | null;
    /** 有利な側（"sente" | "gote" | null） */
    advantage: "sente" | "gote" | null;
} {
    // 詰みの場合（先手視点に正規化済み）
    if (evalMate !== undefined && evalMate !== null) {
        // 符号だけで判定: > 0 なら先手勝ち、< 0 なら後手勝ち
        const winningSide = evalMate > 0 ? "sente" : "gote";
        const winnerMark = winningSide === "sente" ? "☗" : "☖";
        const winnerName = winningSide === "sente" ? "先手" : "後手";

        return {
            description: `${winnerMark}${winnerName}の勝ち`,
            detail: `${Math.abs(evalMate)}手詰み`,
            depthText: depth !== undefined ? `深さ${depth}` : null,
            advantage: winningSide,
        };
    }

    // 通常の評価値
    if (evalCp !== undefined && evalCp !== null) {
        const absValue = Math.abs(evalCp);
        const isSenteAdvantage = evalCp >= 0;
        const mark = isSenteAdvantage ? "☗" : "☖";
        const sideName = isSenteAdvantage ? "先手" : "後手";

        // 優勢度の表現
        let levelText: string;
        if (absValue < 100) {
            levelText = "互角";
        } else if (absValue < 300) {
            levelText = `${sideName}やや有利`;
        } else if (absValue < 600) {
            levelText = `${sideName}有利`;
        } else if (absValue < 1000) {
            levelText = `${sideName}優勢`;
        } else {
            levelText = `${sideName}勝勢`;
        }

        const description = absValue < 100 ? levelText : `${mark}${levelText}`;

        return {
            description,
            detail: `${evalCp >= 0 ? "+" : ""}${evalCp}cp`,
            depthText: depth !== undefined ? `深さ${depth}` : null,
            advantage: absValue < 100 ? null : isSenteAdvantage ? "sente" : "gote",
        };
    }

    return {
        description: "評価なし",
        detail: "",
        depthText: null,
        advantage: null,
    };
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

/** multiPvEvals変換用の入力データ（KifuNode.multiPvEvalsから正規化後） */
export interface MultiPvNodeData {
    scoreCp?: number;
    scoreMate?: number;
    depth?: number;
    pv?: string[];
}

/** 評価値と消費時間を含むノードデータ */
export interface NodeData {
    scoreCp?: number;
    scoreMate?: number;
    depth?: number;
    elapsedMs?: number;
    pv?: string[];
    /** 複数PV用の評価値配列（インデックス=multipv-1） */
    multiPvEvals?: (MultiPvNodeData | undefined)[];
}

/**
 * 複数の指し手をKIF形式に一括変換
 *
 * @param moves USI形式の指し手配列
 * @param boardHistory 各手直前の盤面状態の配列（moves と同じ長さ）
 * @param nodeDataMap 手数 → 評価値・消費時間のマップ（オプション）
 * @returns KifMove の配列
 */
export function convertMovesToKif(
    moves: string[],
    boardHistory: BoardState[],
    nodeDataMap?: Map<number, NodeData>,
): KifMove[] {
    const kifMoves: KifMove[] = [];
    let prevTo: Square | undefined;

    for (let i = 0; i < moves.length; i++) {
        const ply = i + 1;
        const turn: Player = i % 2 === 0 ? "sente" : "gote";
        const board = boardHistory[i];
        const nodeData = nodeDataMap?.get(ply);
        const move = moves[i];

        // パス手の場合は盤面履歴がなくても処理可能
        const isPassMove = move?.toLowerCase() === "pass";

        if (!board && !isPassMove) {
            // 盤面履歴がなく、パス手でもない場合はスキップ
            continue;
        }

        // パス手の場合は空の盤面でも処理できる（formatMoveToKif/formatMoveSimpleがパスを特別処理する）
        const effectiveBoard = board ?? ({} as BoardState);
        const kifText = formatMoveToKif(move, turn, effectiveBoard, prevTo);
        const displayText = formatMoveSimple(move, turn, effectiveBoard, prevTo);

        // multiPvEvals を PvEvalInfo[] に変換
        let multiPvEvals: PvEvalInfo[] | undefined;
        if (nodeData?.multiPvEvals && nodeData.multiPvEvals.length > 0) {
            multiPvEvals = [];
            for (let mpvIndex = 0; mpvIndex < nodeData.multiPvEvals.length; mpvIndex++) {
                const mpvData = nodeData.multiPvEvals[mpvIndex];
                if (mpvData) {
                    multiPvEvals.push({
                        multipv: mpvIndex + 1,
                        evalCp: mpvData.scoreCp,
                        evalMate: mpvData.scoreMate,
                        depth: mpvData.depth,
                        pv: mpvData.pv,
                    });
                }
            }
            // 空配列の場合は undefined にする
            if (multiPvEvals.length === 0) {
                multiPvEvals = undefined;
            }
        }

        kifMoves.push({
            ply,
            kifText,
            displayText,
            usiMove: moves[i],
            evalCp: nodeData?.scoreCp,
            evalMate: nodeData?.scoreMate,
            depth: nodeData?.depth,
            elapsedMs: nodeData?.elapsedMs,
            pv: nodeData?.pv,
            multiPvEvals,
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
    /** 評価値をコメントとして出力するか */
    includeEval?: boolean;
    /** 開始局面（SFEN形式） */
    startSfen?: string;
}

/**
 * ミリ秒を KIF 形式の時間表記に変換
 * @param ms ミリ秒
 * @returns "m:ss" 形式（例: "0:05", "1:30"）
 */
function formatMoveTime(ms: number): string {
    const totalSeconds = Math.floor(ms / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/**
 * ミリ秒を累計時間表記に変換
 * @param ms ミリ秒
 * @returns "hh:mm:ss" 形式（例: "00:00:05", "01:30:00"）
 */
function formatCumulativeTime(ms: number): string {
    const totalSeconds = Math.floor(ms / 1000);
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;
    return `${hours.toString().padStart(2, "0")}:${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
}

/**
 * 1手をKIFファイル形式でフォーマット
 *
 * @param ply 手数
 * @param usiMove USI形式の指し手
 * @param board 指し手適用前の盤面
 * @param prevTo 直前の移動先マス（「同」表記判定用）
 * @param elapsedMs 消費時間（ミリ秒）
 * @param cumulativeMs 累計時間（ミリ秒）
 * @returns KIFファイル形式の1行（例: "   1 ２六歩(27)   ( 0:05/00:00:05)"）
 */
function formatMoveForKifFile(
    ply: number,
    usiMove: string,
    board: BoardState,
    prevTo?: Square,
    elapsedMs?: number,
    cumulativeMs?: number,
): string {
    const plyStr = ply.toString().padStart(4, " ");
    const moveTime = formatMoveTime(elapsedMs ?? 0);
    const cumTime = formatCumulativeTime(cumulativeMs ?? 0);
    const timeStr = `(${moveTime.padStart(5, " ")}/${cumTime})`;

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
        return `${plyStr} ${toKanji}${pieceName}打     ${timeStr}`;
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
    const toKanji = prevTo === to ? SAME_SQUARE_TEXT : squareToKanji(to);

    // 駒名を取得
    const pieceName = getPieceName(piece.type, piece.promoted ?? false);

    // 成り表記
    const promoteText = promotes ? "成" : "";

    // 移動元を数字で表示
    const fromDigits = squareToDigits(from);

    return `${plyStr} ${toKanji}${pieceName}${promoteText}(${fromDigits})   ${timeStr}`;
}

/**
 * 評価値をコメント形式でフォーマット
 * @param evalCp 評価値（センチポーン）
 * @param evalMate 詰み手数
 * @param depth 探索深さ
 * @returns コメント文字列（例: "*評価値=+50 (深さ20)"）
 */
function formatEvalComment(evalCp?: number, evalMate?: number, depth?: number): string | null {
    if (evalCp === undefined && evalMate === undefined) {
        return null;
    }

    let evalStr: string;
    if (evalMate !== undefined && evalMate !== null) {
        if (evalMate > 0) {
            evalStr = `詰${evalMate}手`;
        } else {
            evalStr = `被詰${Math.abs(evalMate)}手`;
        }
    } else if (evalCp !== undefined && evalCp !== null) {
        const value = evalCp / 100;
        evalStr = value >= 0 ? `+${value.toFixed(1)}` : value.toFixed(1);
    } else {
        return null;
    }

    const depthStr = depth !== undefined ? ` (深さ${depth})` : "";
    return `*評価値=${evalStr}${depthStr}`;
}

/**
 * 完全なKIF形式の文字列を生成（クリップボードコピー用）
 *
 * @param kifMoves KIF形式の指し手配列（評価値・消費時間含む）
 * @param boardHistory 各手直前の盤面状態の配列
 * @param options エクスポートオプション
 * @returns KIF形式の完全な文字列
 */
export function exportToKifString(
    kifMoves: KifMove[],
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

    const normalizedStartSfen = normalizeStartSfen(options.startSfen);
    if (normalizedStartSfen && normalizedStartSfen !== HIRATE_SFEN) {
        lines.push(`開始局面：${normalizedStartSfen}`);
    }

    // 指し手ヘッダー
    lines.push("手数----指手---------消費時間--");

    // 各手
    let prevTo: Square | undefined;
    // 先手・後手の累計時間を追跡
    let senteCumulativeMs = 0;
    let goteCumulativeMs = 0;

    for (let i = 0; i < kifMoves.length; i++) {
        const move = kifMoves[i];
        const ply = move.ply;
        const board = boardHistory[i];

        if (!board) continue;

        // 累計時間を計算（奇数手=先手、偶数手=後手）
        const isSenteMove = ply % 2 !== 0;
        const elapsedMs = move.elapsedMs ?? 0;
        if (isSenteMove) {
            senteCumulativeMs += elapsedMs;
        } else {
            goteCumulativeMs += elapsedMs;
        }
        const cumulativeMs = isSenteMove ? senteCumulativeMs : goteCumulativeMs;

        const moveLine = formatMoveForKifFile(
            ply,
            move.usiMove,
            board,
            prevTo,
            elapsedMs,
            cumulativeMs,
        );
        lines.push(moveLine);

        // 評価値コメント（オプションで有効な場合）
        if (options.includeEval) {
            const evalComment = formatEvalComment(move.evalCp, move.evalMate, move.depth);
            if (evalComment) {
                lines.push(evalComment);
            }
        }

        prevTo = parseToSquare(move.usiMove);
    }

    // 終局表示（手数がある場合）
    if (kifMoves.length > 0) {
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

// ============================================================
// PV（読み筋）変換機能
// ============================================================

/** PV変換結果 */
export interface PvDisplayMove {
    /** USI形式の指し手 */
    usiMove: string;
    /** 簡易表示用文字列（例: "☗7六歩"） */
    displayText: string;
    /** 手番 */
    turn: Player;
}

/**
 * USI形式のPVを表示用に変換
 *
 * @param pv USI形式の指し手配列（パス手 "pass" を含む可能性あり）
 * @param position 現在局面（PV開始時点の局面）
 * @returns 表示用PV配列
 */
export function convertPvToDisplay(pv: string[], position: PositionState): PvDisplayMove[] {
    const result: PvDisplayMove[] = [];
    let currentPosition = position;
    let prevTo: Square | undefined;

    for (const usiMove of pv) {
        const turn = currentPosition.turn;
        const board = currentPosition.board;

        // 簡易表示形式に変換（パス手も正しく処理される）
        const displayText = formatMoveSimple(usiMove, turn, board, prevTo);

        result.push({
            usiMove,
            displayText,
            turn,
        });

        // 次の手のために局面を進める
        const moveResult = applyMoveWithState(currentPosition, usiMove, { validateTurn: false });
        if (!moveResult.ok) {
            // 無効な手の場合は残りを処理せずに終了
            break;
        }
        currentPosition = moveResult.next;
        // パス手の場合は prevTo をリセット（パス後の手では「同」表記を使わない）
        if (usiMove.toLowerCase() === "pass") {
            prevTo = undefined;
        } else {
            prevTo = parseToSquare(usiMove);
        }
    }

    return result;
}
