/**
 * KIF形式パーサー
 *
 * KIF形式の棋譜をパースしてUSI形式の指し手配列に変換する
 */

// ============================================================
// 漢数字逆引きテーブル
// ============================================================

/** 筋（ファイル）の漢数字 → 数字 */
const FILE_KANJI_TO_NUM: Readonly<Record<string, string>> = {
    "１": "1",
    "２": "2",
    "３": "3",
    "４": "4",
    "５": "5",
    "６": "6",
    "７": "7",
    "８": "8",
    "９": "9",
    // 半角数字もサポート
    "1": "1",
    "2": "2",
    "3": "3",
    "4": "4",
    "5": "5",
    "6": "6",
    "7": "7",
    "8": "8",
    "9": "9",
};

/** 段（ランク）の漢数字 → アルファベット */
const RANK_KANJI_TO_ALPHA: Readonly<Record<string, string>> = {
    一: "a",
    二: "b",
    三: "c",
    四: "d",
    五: "e",
    六: "f",
    七: "g",
    八: "h",
    九: "i",
};

/** 日本語駒名 → USI駒文字 */
const PIECE_NAME_TO_USI: Readonly<Record<string, string>> = {
    歩: "P",
    香: "L",
    桂: "N",
    銀: "S",
    金: "G",
    角: "B",
    飛: "R",
    玉: "K",
    王: "K",
    // 成り駒
    と: "P",
    成香: "L",
    成桂: "N",
    成銀: "S",
    馬: "B",
    龍: "R",
    竜: "R",
};

// ============================================================
// パース結果の型
// ============================================================

/** 1手のパースデータ */
export interface KifMoveData {
    /** USI形式の指し手 */
    usiMove: string;
    /** 消費時間（ミリ秒） */
    elapsedMs?: number;
    /** 評価値（センチポーン） */
    evalCp?: number;
    /** 詰み手数 */
    evalMate?: number;
    /** 探索深さ */
    depth?: number;
}

interface KifParseResult {
    /** 成功したか */
    success: boolean;
    /** USI形式の指し手配列 */
    moves: string[];
    /** 各手の詳細データ（消費時間・評価値含む） */
    moveData: KifMoveData[];
    /** 開始局面のSFEN（KIFに記載がある場合のみ） */
    startSfen?: string;
    /** エラーメッセージ（失敗時） */
    error?: string;
    /** パースできなかった行（警告用） */
    warnings?: string[];
}

// ============================================================
// パーサー関数
// ============================================================

/**
 * 移動元座標（数字2桁）をUSI形式に変換
 * @param digits "77" のような数字2桁
 * @returns "7g" のようなUSI形式、またはnull
 */
function digitsToUsi(digits: string): string | null {
    if (digits.length !== 2) return null;

    const file = digits[0];
    const rank = parseInt(digits[1], 10);

    if (rank < 1 || rank > 9) return null;

    const rankAlpha = String.fromCharCode(96 + rank); // 1='a', 2='b', ..., 9='i'
    return `${file}${rankAlpha}`;
}

/**
 * KIF形式のマス座標（漢数字）をUSI形式に変換
 * @param fileChar 筋の文字（"７" など）
 * @param rankChar 段の文字（"六" など）
 * @returns "7f" のようなUSI形式、またはnull
 */
function kanjiToUsi(fileChar: string, rankChar: string): string | null {
    const file = FILE_KANJI_TO_NUM[fileChar];
    const rankAlpha = RANK_KANJI_TO_ALPHA[rankChar];

    if (!file || !rankAlpha) return null;

    return `${file}${rankAlpha}`;
}

/** 指し手行のパース結果 */
interface ParsedMoveLine {
    /** USI形式の指し手 */
    usiMove: string;
    /** 移動先マス（次の「同」判定用） */
    toSquare: string;
    /** 消費時間（ミリ秒） */
    elapsedMs?: number;
}

/**
 * 時間表記をパースしてミリ秒に変換
 * @param timeStr "( 0:05/00:00:05)" または "0:05" 形式
 * @returns ミリ秒、またはundefined
 */
function parseTimeNotation(timeStr: string): number | undefined {
    // "( m:ss/hh:mm:ss)" 形式から最初の m:ss を抽出
    const match = timeStr.match(/\(\s*(\d+):(\d+)\/[\d:]+\)/);
    if (match) {
        const minutes = parseInt(match[1], 10);
        const seconds = parseInt(match[2], 10);
        return (minutes * 60 + seconds) * 1000;
    }
    return undefined;
}

/**
 * 評価値コメントをパース
 * @param line "*評価値=+0.5 (深さ20)" 形式
 * @returns { evalCp, evalMate, depth } またはnull
 */
function parseEvalComment(
    line: string,
): { evalCp?: number; evalMate?: number; depth?: number } | null {
    const trimmed = line.trim();
    if (!trimmed.startsWith("*評価値=")) {
        return null;
    }

    const content = trimmed.slice(5); // "*評価値=" を除去

    // 探索深さを抽出
    let depth: number | undefined;
    const depthMatch = content.match(/\(深さ(\d+)\)/);
    if (depthMatch) {
        depth = parseInt(depthMatch[1], 10);
    }

    // 詰み手数: "詰3手" or "被詰5手"
    const mateMatch = content.match(/^(詰|被詰)(\d+)手/);
    if (mateMatch) {
        const mateValue = parseInt(mateMatch[2], 10);
        const evalMate = mateMatch[1] === "詰" ? mateValue : -mateValue;
        return { evalMate, depth };
    }

    // 評価値: "+0.5" or "-1.2"
    const evalMatch = content.match(/^([+-]?\d+\.?\d*)/);
    if (evalMatch) {
        const evalValue = parseFloat(evalMatch[1]);
        // センチポーンに変換（100倍）、極端な値はクランプ（±100000cp = ±1000）
        const rawCp = Math.round(evalValue * 100);
        const evalCp = Math.max(-10000000, Math.min(10000000, rawCp));
        return { evalCp, depth };
    }

    return null;
}

/**
 * 開始局面行をパースしてSFENを取得
 * @param line KIFヘッダー行
 * @returns SFEN文字列またはnull
 */
function parseStartSfenLine(line: string): string | null {
    const trimmed = line.trim();
    const match = trimmed.match(/^開始局面[:：]\s*(.+)$/);
    if (!match) return null;

    const raw = match[1].trim();
    if (!raw) return null;

    const parsed = parseSfen(raw);
    return parsed.sfen || null;
}

/**
 * 1行のKIF指し手をパースしてUSI形式に変換
 *
 * 対応フォーマット:
 * - "1 ７六歩(77)" - 手数付き
 * - "▲７六歩(77)" - 先手マーク付き
 * - "△３四歩(33)" - 後手マーク付き
 * - "７六歩(77)" - マークなし
 * - "同　歩(66)" - 「同」表記
 * - "５五角打" - 駒打ち
 * - "２二角成(88)" - 成り
 * - "   1 ７六歩(77)   ( 0:05/00:00:05)" - 消費時間付き
 *
 * @param line KIF形式の1行
 * @param prevTo 直前の移動先（「同」表記解決用）
 * @returns ParsedMoveLine または null
 */
function parseKifLine(line: string, prevTo: string | null): ParsedMoveLine | null {
    // 空行やコメント行をスキップ
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#") || trimmed.startsWith("*")) {
        return null;
    }

    // ヘッダー行をスキップ
    if (
        trimmed.startsWith("手数") ||
        trimmed.startsWith("開始") ||
        trimmed.startsWith("終了") ||
        trimmed.startsWith("手合") ||
        trimmed.startsWith("先手") ||
        trimmed.startsWith("後手") ||
        trimmed.startsWith("場所") ||
        trimmed.startsWith("持ち時間") ||
        trimmed.startsWith("表題") ||
        trimmed.startsWith("棋戦") ||
        trimmed.includes("version=") ||
        trimmed.includes("encoding=")
    ) {
        return null;
    }

    // 投了・中断などの終局表記をスキップ
    if (
        trimmed.includes("投了") ||
        trimmed.includes("中断") ||
        trimmed.includes("千日手") ||
        trimmed.includes("持将棋") ||
        trimmed.includes("反則") ||
        trimmed.includes("切れ負け") ||
        trimmed.includes("入玉")
    ) {
        return null;
    }

    // 消費時間を抽出（除去前に）
    const elapsedMs = parseTimeNotation(trimmed);

    // 手数を除去: "   1 ７六歩(77)" → "７六歩(77)"
    // 先手後手マークも除去: "▲７六歩(77)" → "７六歩(77)"
    let moveStr = trimmed
        .replace(/^\s*\d+\s+/, "") // 手数除去
        .replace(/^[▲△☗☖]\s*/, "") // 先手後手マーク除去
        .replace(/\s*\([^)]*\)\s*$/, (match) => match.trim()); // 時間表記は残す

    // 時間表記 "( 0:00/00:00:00)" を除去
    moveStr = moveStr.replace(/\s*\(\s*\d+:\d+\/[\d:]+\)\s*$/, "");

    if (!moveStr) return null;

    // ============================================================
    // 駒打ちのパース: "５五角打"
    // 駒名を明示的に列挙して早期にエラー検出
    // ============================================================
    const dropMatch = moveStr.match(
        /^([１２３４５６７８９1-9])([一二三四五六七八九])(歩|香|桂|銀|金|角|飛)打$/,
    );
    if (dropMatch) {
        const [, fileChar, rankChar, pieceName] = dropMatch;
        const to = kanjiToUsi(fileChar, rankChar);
        const piece = PIECE_NAME_TO_USI[pieceName];

        if (to && piece) {
            return { usiMove: `${piece}*${to}`, toSquare: to, elapsedMs };
        }
        return null;
    }

    // ============================================================
    // 「同」表記のパース: "同　歩(66)" or "同歩(66)"
    // 駒名部分は検証不要（後続の PIECE_NAME_TO_USI で検証）、移動元座標のみ抽出
    // ============================================================
    const sameMatch = moveStr.match(/^同[　\s]*(?:.+?)(?:成)?(?:\((\d{2})\))?$/);
    if (sameMatch && prevTo) {
        const [, fromDigits] = sameMatch;
        const promotes =
            moveStr.includes("成") &&
            !moveStr.includes("成香") &&
            !moveStr.includes("成桂") &&
            !moveStr.includes("成銀");

        if (!fromDigits) {
            // 移動元がない場合はパースできない
            return null;
        }

        const from = digitsToUsi(fromDigits);
        if (!from) return null;

        const usiMove = promotes ? `${from}${prevTo}+` : `${from}${prevTo}`;
        return { usiMove, toSquare: prevTo, elapsedMs };
    }

    // ============================================================
    // 通常移動のパース: "７六歩(77)" or "２二角成(88)"
    // ============================================================
    const normalMatch = moveStr.match(
        /^([１２３４５６７８９1-9])([一二三四五六七八九])(.+?)(?:成)?(?:\((\d{2})\))?$/,
    );
    if (normalMatch) {
        const [, fileChar, rankChar, , fromDigits] = normalMatch;
        const promotes =
            moveStr.includes("成") &&
            !moveStr.includes("成香") &&
            !moveStr.includes("成桂") &&
            !moveStr.includes("成銀");

        const to = kanjiToUsi(fileChar, rankChar);
        if (!to) return null;

        if (!fromDigits) {
            // 移動元がない場合はパースできない（盤面情報がないと解決できない）
            return null;
        }

        const from = digitsToUsi(fromDigits);
        if (!from) return null;

        const usiMove = promotes ? `${from}${to}+` : `${from}${to}`;
        return { usiMove, toSquare: to, elapsedMs };
    }

    return null;
}

/**
 * KIF形式の棋譜全体をパースしてUSI形式の指し手配列に変換
 *
 * 消費時間と評価値コメントもパースして返す。
 * 評価値コメントは直前の指し手に紐付けられる。
 *
 * @param kifText KIF形式の棋譜テキスト
 * @returns パース結果
 */
export function parseKif(kifText: string): KifParseResult {
    if (!kifText || !kifText.trim()) {
        return {
            success: false,
            moves: [],
            moveData: [],
            startSfen: undefined,
            error: "入力が空です",
        };
    }

    const lines = kifText.split(/\r?\n/);
    const moves: string[] = [];
    const moveData: KifMoveData[] = [];
    const warnings: string[] = [];
    let prevTo: string | null = null;
    let startSfen: string | undefined;

    for (const line of lines) {
        const startSfenFromLine = parseStartSfenLine(line);
        if (startSfenFromLine && !startSfen) {
            startSfen = startSfenFromLine;
            continue;
        }

        // まず評価値コメントかチェック
        const evalData = parseEvalComment(line);
        if (evalData) {
            // 直前の指し手に評価値を追加
            if (moveData.length > 0) {
                const lastMoveData = moveData[moveData.length - 1];
                if (evalData.evalCp !== undefined) {
                    lastMoveData.evalCp = evalData.evalCp;
                }
                if (evalData.evalMate !== undefined) {
                    lastMoveData.evalMate = evalData.evalMate;
                }
                if (evalData.depth !== undefined) {
                    lastMoveData.depth = evalData.depth;
                }
            }
            continue;
        }

        // 指し手行をパース
        const result = parseKifLine(line, prevTo);
        if (result) {
            moves.push(result.usiMove);
            moveData.push({
                usiMove: result.usiMove,
                elapsedMs: result.elapsedMs,
            });
            prevTo = result.toSquare;
        }
    }

    if (moves.length === 0) {
        return {
            success: false,
            moves: [],
            moveData: [],
            startSfen,
            error: "パースできる指し手が見つかりませんでした",
        };
    }

    return {
        success: true,
        moves,
        moveData,
        startSfen,
        warnings: warnings.length > 0 ? warnings : undefined,
    };
}

/**
 * SFEN文字列をパースして開始局面と指し手に分離
 *
 * 対応フォーマット:
 * - "startpos" - 平手初期局面
 * - "startpos moves 7g7f 3c3d" - 平手初期局面 + 指し手
 * - "lnsgkgsnl/..." - SFEN局面のみ
 * - "lnsgkgsnl/... moves 7g7f" - SFEN局面 + 指し手
 * - "sfen lnsgkgsnl/..." - "sfen"キーワード付き
 * - "position startpos moves 7g7f" - "position"キーワード付き
 *
 * @param sfenText SFEN文字列
 * @returns { sfen: 開始局面SFEN, moves: 指し手配列 }
 */
export function parseSfen(sfenText: string): {
    sfen: string;
    moves: string[];
} {
    const trimmed = sfenText.trim();

    if (!trimmed) {
        return { sfen: "", moves: [] };
    }

    // "position" キーワードを除去
    let text = trimmed.replace(/^position\s+/i, "");

    // "sfen" キーワードを除去
    text = text.replace(/^sfen\s+/i, "");

    // "moves" で分割
    const movesIndex = text.indexOf(" moves ");
    if (movesIndex >= 0) {
        const sfenPart = text.slice(0, movesIndex).trim();
        const movesPart = text.slice(movesIndex + 7).trim();
        const moves = movesPart.split(/\s+/).filter((m) => m.length > 0);

        // startpos を標準SFENに変換
        const sfen =
            sfenPart === "startpos"
                ? "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
                : sfenPart;

        return { sfen, moves };
    }

    // moves がない場合
    if (text === "startpos") {
        return {
            sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            moves: [],
        };
    }

    // SFEN局面のみ（moves なし）
    // ただし、スペース区切りの指し手だけが入力された場合を考慮
    // SFENは通常 "/" を含むので、それで判定
    if (text.includes("/")) {
        return { sfen: text, moves: [] };
    }

    // 指し手のみの場合（SFENを含まない）
    const possibleMoves = text.split(/\s+/).filter((m) => m.length > 0);
    // USI形式の指し手かどうかを簡易チェック
    const looksLikeMoves = possibleMoves.every(
        (m) => /^[1-9][a-i][1-9][a-i]\+?$/.test(m) || /^[PLNSGBRK]\*[1-9][a-i]$/.test(m),
    );

    if (looksLikeMoves && possibleMoves.length > 0) {
        return {
            sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            moves: possibleMoves,
        };
    }

    // SFENとして解釈
    return { sfen: text, moves: [] };
}
