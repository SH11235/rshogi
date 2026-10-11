//! 外部棋譜取り込みの純粋ロジック。
//!
//! 管理 API (`POST /api/v1/admin/games/import-wdoor` / `.../upload`) が受け取った
//! CSA 本文から、`game_id` と viewer 配信用メタ ([`GamesIndexEntry`]) を導出する。
//! I/O (wdoor への fetch、R2 / D1 への書き込み) は `kifu_import_api` 側に置き、
//! 本モジュールはホスト target の `cargo test` で検証できる関数だけを持つ。
//!
//! # 取り込み元
//!
//! - `wdoor`: wdoor の Floodgate (shogi-server) が公開する CSA。対局 ID
//!   (`wdoor+<ゲーム名>+<先手>+<後手>+<YYYYMMDDHHMMSS>`) から取得先 URL と
//!   `game_id` が決定的に決まる。
//! - `upload`: 管理者が本文を直接送る棋譜。`game_id` は本文の SHA-256 から決める。
//!
//! # 時刻
//!
//! CSA の `$START_TIME` / `$END_TIME` にはタイムゾーン表記がない。取り込み対象は
//! 日本の対局サーバー・大会の棋譜なので JST として解釈する。

use chrono::NaiveDate;
use rshogi_core::position::Position;
use rshogi_core::types::Color as CoreColor;
use rshogi_csa::{ParsedMove, parse_csa_full};
use rshogi_csa_server::game::validator::Validator;
use rshogi_csa_server::types::CsaMoveToken;
use sha2::{Digest, Sha256};

use crate::config::DEFAULT_ENTERING_KING_RULE;
use crate::games_index::{ClockSpec, GamesIndexEntry, SOURCE_UPLOAD, SOURCE_WDOOR};

/// 取り込む CSA 本文のサイズ上限 (512KB)。
pub const MAX_CSA_BYTES: usize = 512 * 1024;

/// `upload` の `event` に指定できる文字数の上限。
pub const MAX_EVENT_CHARS: usize = 100;

/// wdoor の公開棋譜を取得する固定 origin + path prefix。
///
/// 取得先はここに日付と対局 ID を足した URL だけで、リクエスト側から host や
/// path の階層を差し替える手段を持たせない。
const WDOOR_KIFU_BASE_URL: &str = "https://wdoor.c.u-tokyo.ac.jp/shogi/x";

/// JST (UTC+9) のオフセット (ミリ秒)。
const JST_OFFSET_MS: i64 = 9 * 60 * 60 * 1000;

/// wdoor (shogi-server) の対局 ID。
///
/// `^wdoor\+[A-Za-z0-9_.-]+\+[^+/]+\+[^+/]+\+[0-9]{14}$` に合い、かつ末尾 14 桁が
/// 実在する日時であるものだけを表す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WdoorGameId<'a> {
    raw: &'a str,
    game_name: &'a str,
    timestamp: &'a str,
    started_at_ms: u64,
}

impl<'a> WdoorGameId<'a> {
    /// 対局 ID 文字列を検証して分解する。形式に合わなければ `None`。
    pub fn parse(raw: &'a str) -> Option<Self> {
        let mut parts = raw.split('+');
        let (prefix, game_name, black, white, timestamp) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || prefix != "wdoor" {
            return None;
        }
        let game_name_ok = !game_name.is_empty()
            && game_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        let name_ok = |name: &str| !name.is_empty() && !name.contains('/');
        if !game_name_ok || !name_ok(black) || !name_ok(white) {
            return None;
        }
        let started_at_ms = parse_compact_jst_datetime(timestamp)?;
        Some(Self {
            raw,
            game_name,
            timestamp,
            started_at_ms,
        })
    }

    /// 元の対局 ID。
    pub fn as_str(&self) -> &'a str {
        self.raw
    }

    /// ゲーム名 (例 `floodgate-300-10F`)。メタの `event` になる。
    pub fn game_name(&self) -> &'a str {
        self.game_name
    }

    /// 保存用の `game_id` (`wdoor-<開始時刻>-<sha256(元 ID) の先頭 10 hex>`)。
    pub fn game_id(&self) -> String {
        format!("wdoor-{}-{}", self.timestamp, &sha256_hex(self.raw.as_bytes())[..10])
    }

    /// wdoor の公開 CSA の URL。host と path prefix は固定で、ID は 1 つの path
    /// segment として percent-encode して埋める。
    pub fn kifu_url(&self) -> String {
        let (year, rest) = self.timestamp.split_at(4);
        let (month, rest) = rest.split_at(2);
        let day = &rest[..2];
        format!(
            "{WDOOR_KIFU_BASE_URL}/{year}/{month}/{day}/{}.csa",
            encode_wdoor_path_segment(self.raw)
        )
    }
}

/// wdoor の対局 ID を URL の 1 path segment に埋める。
///
/// wdoor は `+` をそのまま含むファイル名で公開しているため `+` は残す。それ以外は
/// RFC 3986 の unreserved 文字だけを素通しし、`?` `#` `%` や空白などは `%XX` に
/// する (query / fragment として解釈されて取得先が変わるのを防ぐ)。
fn encode_wdoor_path_segment(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// `upload` の `game_id` (`upload-<sha256(CSA 本文) の先頭 16 hex>`)。
pub fn upload_game_id(csa: &[u8]) -> String {
    format!("upload-{}", &sha256_hex(csa)[..16])
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// JST の壁時計時刻を UNIX エポック ミリ秒へ変換する。エポック以前は `None`。
fn jst_to_epoch_ms(date: NaiveDate, hour: u32, minute: u32, second: u32) -> Option<u64> {
    let local_ms = date.and_hms_opt(hour, minute, second)?.and_utc().timestamp_millis();
    u64::try_from(local_ms - JST_OFFSET_MS).ok()
}

/// `YYYYMMDDHHMMSS` (JST) をエポック ミリ秒へ変換する。
fn parse_compact_jst_datetime(s: &str) -> Option<u64> {
    if s.len() != 14 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let num = |range: std::ops::Range<usize>| s[range].parse::<u32>().ok();
    let date = NaiveDate::from_ymd_opt(s[0..4].parse().ok()?, num(4..6)?, num(6..8)?)?;
    jst_to_epoch_ms(date, num(8..10)?, num(10..12)?, num(12..14)?)
}

/// CSA の `YYYY/MM/DD HH:MM:SS` または `YYYY/MM/DD` (JST) をエポック ミリ秒へ変換する。
fn parse_csa_jst_datetime(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date_part, time_part) = match s.split_once(' ') {
        Some((date, time)) => (date, Some(time.trim())),
        None => (s, None),
    };
    let mut ymd = date_part.split('/');
    let date = NaiveDate::from_ymd_opt(
        ymd.next()?.parse().ok()?,
        ymd.next()?.parse().ok()?,
        ymd.next()?.parse().ok()?,
    )?;
    if ymd.next().is_some() {
        return None;
    }
    let (hour, minute, second) = match time_part {
        None => (0, 0, 0),
        Some(time) => {
            let mut hms = time.split(':');
            let parsed =
                (hms.next()?.parse().ok()?, hms.next()?.parse().ok()?, hms.next()?.parse().ok()?);
            if hms.next().is_some() {
                return None;
            }
            parsed
        }
    };
    jst_to_epoch_ms(date, hour, minute, second)
}

/// 取り込んだ棋譜の持ち時間。秒単位の 2 方式だけを判定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportedClock {
    /// 持ち時間 + 秒読み。
    Countdown { total_sec: u32, byoyomi_sec: u32 },
    /// 持ち時間 + 1 手ごとの加算。
    Fischer { total_sec: u32, increment_sec: u32 },
}

impl ImportedClock {
    fn to_wire(self) -> ClockSpec<'static> {
        let (kind, total_sec, byoyomi_sec, increment_sec) = match self {
            Self::Countdown {
                total_sec,
                byoyomi_sec,
            } => ("countdown", total_sec, Some(byoyomi_sec), None),
            Self::Fischer {
                total_sec,
                increment_sec,
            } => ("fischer", total_sec, None, Some(increment_sec)),
        };
        ClockSpec {
            kind,
            total_sec: Some(total_sec),
            byoyomi_sec,
            byoyomi_ms: None,
            increment_sec,
            total_ms: None,
            total_min: None,
            byoyomi_min: None,
        }
    }
}

/// shogi-server のゲーム名 (`<名前>-<持ち時間秒>-<秒読み秒>` / 末尾 `F` で
/// フィッシャー) から持ち時間を読む。例: `floodgate-300-10F` → 300 秒 + 10 秒加算。
fn clock_from_wdoor_game_name(game_name: &str) -> Option<ImportedClock> {
    let mut parts = game_name.rsplitn(3, '-');
    let (last, total, _name) = (parts.next()?, parts.next()?, parts.next()?);
    let is_plain_number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !is_plain_number(total) {
        return None;
    }
    let total_sec = total.parse().ok()?;
    match last.strip_suffix('F') {
        Some(increment) if is_plain_number(increment) => Some(ImportedClock::Fischer {
            total_sec,
            increment_sec: increment.parse().ok()?,
        }),
        Some(_) => None,
        None if is_plain_number(last) => Some(ImportedClock::Countdown {
            total_sec,
            byoyomi_sec: last.parse().ok()?,
        }),
        None => None,
    }
}

/// `$TIME_LIMIT:HH:MM+SS` (持ち時間 + 秒読み) と `'Increment:<秒>` から持ち時間を読む。
///
/// 加算秒が正ならフィッシャー、そうでなければ秒読み方式。`$TIME_LIMIT` が無い、
/// または読めないときは持ち時間本体が分からないので `None`。
fn clock_from_headers(time_limit: Option<&str>, increment: Option<&str>) -> Option<ImportedClock> {
    let (main, byoyomi) = time_limit?.trim().split_once('+')?;
    let (hours, minutes) = main.split_once(':')?;
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    let byoyomi_sec: u32 = byoyomi.parse().ok()?;
    let total_sec = hours.checked_mul(60)?.checked_add(minutes)?.checked_mul(60)?;
    match increment.and_then(|value| value.trim().parse::<u32>().ok()) {
        Some(increment_sec) if increment_sec > 0 => Some(ImportedClock::Fischer {
            total_sec,
            increment_sec,
        }),
        _ => Some(ImportedClock::Countdown {
            total_sec,
            byoyomi_sec,
        }),
    }
}

/// メタ導出に使う CSA のヘッダ・コメント行。同じ項目が複数あれば最後の行を採る。
#[derive(Debug, Default, PartialEq, Eq)]
struct CsaHeaders<'a> {
    event: Option<&'a str>,
    start_time: Option<&'a str>,
    end_time: Option<&'a str>,
    summary: Option<&'a str>,
    time_limit: Option<&'a str>,
    increment: Option<&'a str>,
    /// 最後の `%` 行 (先頭の `%` と `,T<秒>` を除いた部分)。
    last_special: Option<&'a str>,
}

fn scan_headers(csa: &str) -> CsaHeaders<'_> {
    let mut headers = CsaHeaders::default();
    for line in csa.lines() {
        let s = line.trim();
        if let Some(rest) = s.strip_prefix("$EVENT:") {
            headers.event = Some(rest.trim());
        } else if let Some(rest) = s.strip_prefix("$START_TIME:") {
            headers.start_time = Some(rest);
        } else if let Some(rest) =
            s.strip_prefix("'$END_TIME:").or_else(|| s.strip_prefix("$END_TIME:"))
        {
            headers.end_time = Some(rest);
        } else if let Some(rest) = s.strip_prefix("'summary:") {
            headers.summary = Some(rest);
        } else if let Some(rest) = s.strip_prefix("$TIME_LIMIT:") {
            headers.time_limit = Some(rest);
        } else if let Some(rest) = s.strip_prefix("'Increment:") {
            headers.increment = Some(rest);
        } else if let Some(rest) = s.strip_prefix('%') {
            headers.last_special = Some(rest.split(',').next().unwrap_or(rest).trim());
        }
    }
    headers
}

fn win_kind(black_wins: bool) -> &'static str {
    if black_wins { "WIN_BLACK" } else { "WIN_WHITE" }
}

/// shogi-server の `'summary:<理由>:<先手名> <結果>:<後手名> <結果>` から
/// `(result_kind, end_reason)` を導出する。
///
/// 理由文字列は shogi-server `game_result.rb` の `@log_summary_type` に合わせる。
/// wdoor の実棋譜 (2026 年 6〜7 月の約 2.4 万局) に出現したのは `toryo` / `time up` /
/// `sennichite` / `kachi` / `abnormal` / `illegal move` / `oute_sennichite` /
/// `max_moves` / `oute_kaihimore` の 9 種。`uchifuzume` / `outori` / `illegal kachi` は
/// 実棋譜には出ていないが、shogi-server が反則負けとして出力する語なので `ILLEGAL` に写す。
/// 未知の理由は `ABNORMAL`、勝敗語が読めないときは `ABORT` に倒す。
fn result_from_summary(summary: &str) -> (&'static str, &'static str) {
    let (reason, players) = summary.split_once(':').unwrap_or((summary, ""));
    let end_reason = match reason.trim() {
        "toryo" => "RESIGN",
        "time up" => "TIME_UP",
        "illegal move" | "uchifuzume" | "oute_kaihimore" | "outori" | "illegal kachi" => "ILLEGAL",
        "kachi" => "JISHOGI",
        "oute_sennichite" => "OUTE_SENNICHITE",
        "sennichite" => "SENNICHITE",
        "max_moves" => "MAX_MOVES",
        _ => "ABNORMAL",
    };
    // 対局者名は勝敗判定に使わない (レート付き対局者は `<名前>+<hash>` 形式で
    // 書かれることがある)。先手・後手は並び順で決まる。
    fn outcome(part: &str) -> Option<&str> {
        part.trim().rsplit(' ').next()
    }
    let result_kind = match players.rsplit_once(':') {
        Some((black, white)) => match (outcome(black), outcome(white)) {
            (Some("win"), Some("lose")) => "WIN_BLACK",
            (Some("lose"), Some("win")) => "WIN_WHITE",
            (Some("draw"), Some("draw")) => "DRAW",
            _ => "ABORT",
        },
        None => "ABORT",
    };
    (result_kind, end_reason)
}

/// 棋譜末尾の `%` コマンドと終局時点の手番から `(result_kind, end_reason)` を導出する。
///
/// `black_to_move` は最後の指し手を適用した後の手番。CSA の規定どおり `%TORYO` /
/// `%TIME_UP` は手番側の負け、`%KACHI` / `%ILLEGAL_MOVE` (直前の手が反則) は手番側の
/// 勝ち、`%+ILLEGAL_ACTION` / `%-ILLEGAL_ACTION` は符号の側の負けとする。
fn result_from_special(special: Option<&str>, black_to_move: bool) -> (&'static str, &'static str) {
    match special {
        Some("TORYO") => (win_kind(!black_to_move), "RESIGN"),
        Some("TIME_UP") => (win_kind(!black_to_move), "TIME_UP"),
        Some("ILLEGAL_MOVE") => (win_kind(black_to_move), "ILLEGAL"),
        Some("+ILLEGAL_ACTION") => ("WIN_WHITE", "ILLEGAL"),
        Some("-ILLEGAL_ACTION") => ("WIN_BLACK", "ILLEGAL"),
        Some("KACHI") => (win_kind(black_to_move), "JISHOGI"),
        Some("JISHOGI") => ("DRAW", "JISHOGI"),
        Some("SENNICHITE") => ("DRAW", "SENNICHITE"),
        Some("MAX_MOVES") => ("DRAW", "MAX_MOVES"),
        Some("HIKIWAKE") => ("DRAW", "ABNORMAL"),
        _ => ("ABORT", "ABNORMAL"),
    }
}

/// 初期局面から指し手を合法手として再生した結果。
#[derive(Debug, PartialEq, Eq)]
struct Replay {
    black: String,
    white: String,
    moves_count: u32,
    /// 最後の指し手の後の手番が先手か。
    black_to_move: bool,
    /// 指し手に付いた消費時間の合計 (秒)。
    total_elapsed_sec: u64,
}

/// CSA 本文を parse し、全指し手が合法であることを確かめる。
///
/// 末尾が `%ILLEGAL_MOVE` の棋譜だけは、最後の 1 手が反則手であることを許す
/// (CSA の「直前の手が反則手」の記録形式)。その手は局面に適用せず、手番だけ進める。
fn replay(csa: &str, ends_with_illegal_move: bool) -> Result<Replay, String> {
    let (initial, parsed_moves, info) =
        parse_csa_full(csa).map_err(|e| format!("parse CSA: {e}"))?;
    let name = |value: Option<String>, label: &str| {
        value
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("missing player name ({label})"))
    };
    let black = name(info.black_name, "N+")?;
    let white = name(info.white_name, "N-")?;

    let mut pos = Position::new();
    pos.set_sfen(&initial.to_sfen())
        .map_err(|e| format!("invalid initial position: {e:?}"))?;
    let validator = Validator::new(DEFAULT_ENTERING_KING_RULE);
    let moves: Vec<_> = parsed_moves
        .iter()
        .filter_map(|parsed| match parsed {
            ParsedMove::Normal(mv) => Some(mv),
            ParsedMove::Special(_) => None,
        })
        .collect();
    let mut black_to_move = pos.side_to_move() == CoreColor::Black;
    let mut total_elapsed_sec = 0_u64;
    for (index, mv) in moves.iter().enumerate() {
        match validator.validate_move(&pos, &CsaMoveToken::new(mv.mv.as_str())) {
            Ok(core_move) => {
                let gives_check = pos.gives_check(core_move);
                pos.do_move(core_move, gives_check);
            }
            Err(_) if ends_with_illegal_move && index + 1 == moves.len() => {}
            Err(violation) => {
                return Err(format!("move {} ({}) is not legal: {violation:?}", index + 1, mv.mv));
            }
        }
        black_to_move = !black_to_move;
        total_elapsed_sec += u64::from(mv.time_sec.unwrap_or(0));
    }
    Ok(Replay {
        black,
        white,
        moves_count: moves.len() as u32,
        black_to_move,
        total_elapsed_sec,
    })
}

/// 取り込みが成立しない理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// 終局していない (`'summary:` 行が無い)。wdoor の参照取り込みでのみ返す。
    Unfinished,
    /// `source = wdoor` のアップロードで `$EVENT` が wdoor の対局 ID 形式でない。
    NotWdoorEvent,
    /// 対局者名・開始時刻が読めない、または指し手を初期局面から合法手として再生できない。
    Unplayable(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unfinished => f.write_str("game is not finished (no summary line)"),
            Self::NotWdoorEvent => f.write_str("$EVENT is not a wdoor game id"),
            Self::Unplayable(detail) => write!(f, "kifu cannot be replayed: {detail}"),
        }
    }
}

/// 取り込む 1 局分のメタ。[`ImportedGame::entry`] で wire 形式に変換する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedGame {
    pub game_id: String,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub black: String,
    pub white: String,
    pub result_kind: &'static str,
    pub end_reason: &'static str,
    pub moves_count: u32,
    pub clock: Option<ImportedClock>,
    pub source: &'static str,
    pub event: Option<String>,
    pub origin_url: Option<String>,
}

impl ImportedGame {
    /// R2 meta / games-index / D1 に書く entry。外部棋譜なので player ID は付けない。
    pub fn entry(&self) -> GamesIndexEntry<'_> {
        GamesIndexEntry {
            game_id: &self.game_id,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            black_handle: &self.black,
            white_handle: &self.white,
            black_player_id: None,
            white_player_id: None,
            result_kind: self.result_kind,
            end_reason: self.end_reason,
            moves_count: self.moves_count,
            clock: self.clock.map(ImportedClock::to_wire),
            source: self.source,
            event: self.event.as_deref(),
            origin_url: self.origin_url.as_deref(),
        }
    }
}

/// CSA から読み取った、取り込み元に依らない部分。
struct Derived {
    replay: Replay,
    result: (&'static str, &'static str),
    started_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
}

fn derive(csa: &str, headers: &CsaHeaders<'_>) -> Result<Derived, ImportError> {
    // `'summary:` がある棋譜は反則手を指し手として記録しない (shogi-server は
    // `'ILLEGAL_MOVE(...)` コメントにする) ので、反則手の許容は `%` だけで終わる棋譜に限る。
    let ends_with_illegal_move =
        headers.summary.is_none() && headers.last_special == Some("ILLEGAL_MOVE");
    let replay = replay(csa, ends_with_illegal_move).map_err(ImportError::Unplayable)?;
    let result = match headers.summary {
        Some(summary) => result_from_summary(summary),
        None => result_from_special(headers.last_special, replay.black_to_move),
    };
    Ok(Derived {
        replay,
        result,
        started_at_ms: headers.start_time.and_then(parse_csa_jst_datetime),
        ended_at_ms: headers.end_time.and_then(parse_csa_jst_datetime),
    })
}

/// 終了時刻を決める。`$END_TIME` が無ければ開始時刻 + 消費時間の合計。
/// 開始より前の終了時刻は開始時刻に丸める (一覧の並び順キーなので逆転させない)。
fn resolve_ended_at_ms(started_at_ms: u64, derived: &Derived) -> u64 {
    derived
        .ended_at_ms
        .unwrap_or_else(|| {
            started_at_ms.saturating_add(derived.replay.total_elapsed_sec.saturating_mul(1000))
        })
        .max(started_at_ms)
}

/// wdoor の対局 1 局をメタに変換する。
///
/// `require_summary` は参照取り込み (wdoor から今取得した本文) で `true` にし、
/// 対局中の棋譜を [`ImportError::Unfinished`] として弾く。`origin_url` は wdoor から
/// 実際に取得できたときだけ渡す。
pub fn import_wdoor(
    id: &WdoorGameId<'_>,
    csa: &str,
    origin_url: Option<String>,
    require_summary: bool,
) -> Result<ImportedGame, ImportError> {
    let headers = scan_headers(csa);
    if require_summary && headers.summary.is_none() {
        return Err(ImportError::Unfinished);
    }
    let derived = derive(csa, &headers)?;
    // `$START_TIME` が読めなくても、対局 ID の時刻で代用できる。
    let started_at_ms = derived.started_at_ms.unwrap_or(id.started_at_ms);
    let ended_at_ms = resolve_ended_at_ms(started_at_ms, &derived);
    Ok(ImportedGame {
        game_id: id.game_id(),
        started_at_ms,
        ended_at_ms,
        black: derived.replay.black,
        white: derived.replay.white,
        result_kind: derived.result.0,
        end_reason: derived.result.1,
        moves_count: derived.replay.moves_count,
        clock: clock_from_wdoor_game_name(id.game_name()),
        source: SOURCE_WDOOR,
        event: Some(id.game_name().to_owned()),
        origin_url,
    })
}

/// 手元アーカイブの wdoor 棋譜 (本文アップロード、`source = wdoor`) をメタに変換する。
///
/// `$EVENT` の対局 ID から参照取り込みと同じ `game_id` を決める。wdoor から取得した
/// わけではないので `origin_url` は付けず、`'summary:` 行も必須にしない。
pub fn import_wdoor_archive(csa: &str) -> Result<ImportedGame, ImportError> {
    let id = scan_headers(csa)
        .event
        .and_then(WdoorGameId::parse)
        .ok_or(ImportError::NotWdoorEvent)?;
    import_wdoor(&id, csa, None, false)
}

/// 管理者がアップロードした棋譜 (`source = upload`) をメタに変換する。
///
/// `event` は呼び出し側で正規化済み (空でない、[`MAX_EVENT_CHARS`] 以内) の値を渡す。
pub fn import_upload(csa: &str, event: Option<&str>) -> Result<ImportedGame, ImportError> {
    let headers = scan_headers(csa);
    let derived = derive(csa, &headers)?;
    let started_at_ms = derived
        .started_at_ms
        .ok_or_else(|| ImportError::Unplayable("missing or invalid $START_TIME".to_owned()))?;
    let ended_at_ms = resolve_ended_at_ms(started_at_ms, &derived);
    Ok(ImportedGame {
        game_id: upload_game_id(csa.as_bytes()),
        started_at_ms,
        ended_at_ms,
        black: derived.replay.black,
        white: derived.replay.white,
        result_kind: derived.result.0,
        end_reason: derived.result.1,
        moves_count: derived.replay.moves_count,
        clock: clock_from_headers(headers.time_limit, headers.increment),
        source: SOURCE_UPLOAD,
        event: event.map(str::to_owned),
        origin_url: None,
    })
}

/// `upload` リクエストの `event` を正規化する。前後の空白を除き、空なら `None`。
/// [`MAX_EVENT_CHARS`] を超える、または制御文字を含む値は `Err`。
pub fn normalize_event(raw: Option<&str>) -> Result<Option<&str>, String> {
    let Some(event) = raw.map(str::trim).filter(|event| !event.is_empty()) else {
        return Ok(None);
    };
    if event.chars().count() > MAX_EVENT_CHARS {
        return Err(format!("event must be at most {MAX_EVENT_CHARS} characters"));
    }
    if event.chars().any(char::is_control) {
        return Err("event must not contain control characters".to_owned());
    }
    Ok(Some(event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floodgate_history::validate_key_component;

    const WDOOR_ID: &str = "wdoor+floodgate-300-10F+RAMU+Panda+20261009000001";

    const BOARD: &str = "P1-KY-KE-GI-KI-OU-KI-GI-KE-KY\n\
P2 * -HI *  *  *  *  * -KA * \n\
P3-FU-FU-FU-FU-FU-FU-FU-FU-FU\n\
P4 *  *  *  *  *  *  *  *  * \n\
P5 *  *  *  *  *  *  *  *  * \n\
P6 *  *  *  *  *  *  *  *  * \n\
P7+FU+FU+FU+FU+FU+FU+FU+FU+FU\n\
P8 * +KA *  *  *  *  * +HI * \n\
P9+KY+KE+GI+KI+OU+KI+GI+KE+KY\n\
+\n";

    /// wdoor (shogi-server) が公開する形式の短い棋譜。4 手指して先手が投了。
    fn wdoor_csa() -> String {
        format!(
            "V2\nN+RAMU\nN-Panda\n'Max_Moves:512\n'Least_Time_Per_Move:0\n'Increment:10\n\
$EVENT:{WDOOR_ID}\n$START_TIME:2026/10/09 00:00:00\n{BOARD}\
'rating:RAMU+c902d756:Panda+05db2b40\n'black_rate:RAMU+c902d756:4374\n'white_rate:Panda+05db2b40:3507\n\
+2726FU\nT0\n-8384FU\nT10\n'** 136 +7776FU -4132KI\n+2625FU\nT0\n'** -32\n-4132KI\nT14\n\
%TORYO\n'P1-KY-KE-GI *  * -OU-GI-KE-KY\n'+\n\
'summary:toryo:RAMU lose:Panda win\n'$END_TIME:2026/10/09 00:21:55\n"
        )
    }

    /// 大会棋譜などを想定した、`'summary:` を持たない CSA。
    fn upload_csa(tail: &str) -> String {
        format!(
            "V2.2\nN+alice\nN-bob\n$EVENT:WCSC36\n$START_TIME:2026/05/03 10:00:00\n\
$TIME_LIMIT:00:15+05\nPI\n+\n+7776FU,T3\n-3334FU,T4\n{tail}"
        )
    }

    fn utc_ms(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> u64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, s)
            .unwrap()
            .and_utc()
            .timestamp_millis() as u64
    }

    // ---- wdoor 対局 ID ----

    #[test]
    fn wdoor_id_accepts_contract_examples() {
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        assert_eq!(id.as_str(), WDOOR_ID);
        assert_eq!(id.game_name(), "floodgate-300-10F");
        // 名前には `+` と `/` 以外の任意文字を許す (契約の `[^+/]+`)。
        for raw in [
            "wdoor+floodgate-600-10+a.b_c-d+x y+20260706123456",
            "wdoor+g.1_2-3+名前+b?c#d+20260101000000",
        ] {
            assert!(WdoorGameId::parse(raw).is_some(), "raw={raw}");
        }
    }

    #[test]
    fn wdoor_id_rejects_everything_outside_the_contract_pattern() {
        for raw in [
            "",
            "wdoor",
            "floodgate-300-10F+RAMU+Panda+20261009000001",
            "xdoor+floodgate-300-10F+RAMU+Panda+20261009000001",
            "wdoor+floodgate-300-10F+RAMU+Panda",
            "wdoor+floodgate-300-10F+RAMU+Panda+2026100900000",
            "wdoor+floodgate-300-10F+RAMU+Panda+202610090000011",
            "wdoor+floodgate-300-10F+RAMU+Panda+2026100900000a",
            "wdoor+floodgate-300-10F+RAMU+Panda+20261009000001+extra",
            "wdoor++RAMU+Panda+20261009000001",
            "wdoor+floodgate-300-10F++Panda+20261009000001",
            "wdoor+floodgate-300-10F+RAMU++20261009000001",
            "wdoor+flood gate+RAMU+Panda+20261009000001",
            "wdoor+flood/gate+RAMU+Panda+20261009000001",
            "wdoor+floodgate-300-10F+RA/MU+Panda+20261009000001",
            "wdoor+floodgate-300-10F+RAMU+../../etc+20261009000001",
            "wdoor+floodgate-300-10F+RAMU+Panda+20261009000001\n",
            " wdoor+floodgate-300-10F+RAMU+Panda+20261009000001",
            // 14 桁でも実在しない日時は取得先の日付パスを組めない。
            "wdoor+floodgate-300-10F+RAMU+Panda+20261309000001",
            "wdoor+floodgate-300-10F+RAMU+Panda+20260230000000",
            "wdoor+floodgate-300-10F+RAMU+Panda+20261009250000",
        ] {
            assert!(WdoorGameId::parse(raw).is_none(), "raw={raw:?}");
        }
    }

    #[test]
    fn wdoor_game_id_is_deterministic_and_key_safe() {
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        // sha256(WDOOR_ID) の先頭 10 hex (`printf '%s' <id> | sha256sum` で確認した値)。
        assert_eq!(id.game_id(), "wdoor-20261009000001-93ef4d36a6");
        assert_eq!(id.game_id(), WdoorGameId::parse(WDOOR_ID).unwrap().game_id());
        let other =
            WdoorGameId::parse("wdoor+floodgate-300-10F+Panda+RAMU+20261009000001").unwrap();
        assert_ne!(id.game_id(), other.game_id());
        for raw in [WDOOR_ID, "wdoor+g+名前+b?c#d+20260101000000"] {
            let game_id = WdoorGameId::parse(raw).unwrap().game_id();
            assert!(validate_key_component(&game_id).is_ok(), "game_id={game_id}");
        }
    }

    #[test]
    fn wdoor_url_uses_fixed_host_and_date_path() {
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        assert_eq!(
            id.kifu_url(),
            "https://wdoor.c.u-tokyo.ac.jp/shogi/x/2026/10/09/wdoor+floodgate-300-10F+RAMU+Panda+20261009000001.csa"
        );
    }

    #[test]
    fn wdoor_url_cannot_leave_the_fixed_origin_or_directory() {
        // query / fragment / userinfo / 相対パスに見える名前も 1 segment に閉じ込める。
        for name in [
            "a?x=1",
            "a#frag",
            "a@evil.example",
            "..",
            "a%2Fb",
            "a\\b",
            "a b",
            "名前",
        ] {
            let raw = format!("wdoor+floodgate-300-10F+{name}+Panda+20261009000001");
            let url = WdoorGameId::parse(&raw).unwrap().kifu_url();
            let rest = url
                .strip_prefix("https://wdoor.c.u-tokyo.ac.jp/shogi/x/2026/10/09/")
                .unwrap_or_else(|| panic!("unexpected origin/path: {url}"));
            assert!(rest.ends_with(".csa"), "url={url}");
            assert!(
                rest.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~+%".contains(&b)),
                "segment must be fully encoded: {url}"
            );
        }
        let url = WdoorGameId::parse("wdoor+g+a?b#c+d e+20261009000001").unwrap().kifu_url();
        assert!(url.ends_with("/wdoor+g+a%3Fb%23c+d%20e+20261009000001.csa"), "url={url}");
    }

    #[test]
    fn upload_game_id_is_content_hash_prefix() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223...
        assert_eq!(upload_game_id(b"abc"), "upload-ba7816bf8f01cfea");
        assert_eq!(upload_game_id(b"abc"), upload_game_id(b"abc"));
        assert_ne!(upload_game_id(b"abc"), upload_game_id(b"abc\n"));
        assert!(validate_key_component(&upload_game_id(b"")).is_ok());
    }

    // ---- 時刻 ----

    #[test]
    fn csa_datetime_is_interpreted_as_jst() {
        // JST 2026/10/09 00:00:00 = UTC 2026/10/08 15:00:00
        assert_eq!(
            parse_csa_jst_datetime("2026/10/09 00:00:00"),
            Some(utc_ms(2026, 10, 8, 15, 0, 0))
        );
        assert_eq!(
            parse_csa_jst_datetime(" 2026/10/09 00:21:55 "),
            Some(utc_ms(2026, 10, 8, 15, 21, 55))
        );
        // 日付だけの `$START_TIME` はその日の 00:00:00 JST。
        assert_eq!(parse_csa_jst_datetime("2026/05/03"), Some(utc_ms(2026, 5, 2, 15, 0, 0)));
        assert_eq!(
            parse_compact_jst_datetime("20261009000001"),
            Some(utc_ms(2026, 10, 8, 15, 0, 1))
        );
    }

    #[test]
    fn csa_datetime_rejects_malformed_values() {
        for bad in [
            "",
            "2026-10-09 00:00:00",
            "2026/13/01 00:00:00",
            "2026/10/09 24:00:00",
            "2026/10/09 00:00",
            "2026/10/09 00:00:00:00",
            "2026/10/09/01",
            "1969/12/31 23:59:59",
            "yesterday",
        ] {
            assert_eq!(parse_csa_jst_datetime(bad), None, "bad={bad:?}");
        }
        // JST の 1970/01/01 09:00:00 がエポック 0。
        assert_eq!(parse_csa_jst_datetime("1970/01/01 09:00:00"), Some(0));
        assert_eq!(parse_csa_jst_datetime("1970/01/01 08:59:59"), None);
    }

    // ---- 持ち時間 ----

    #[test]
    fn wdoor_game_name_clock() {
        assert_eq!(
            clock_from_wdoor_game_name("floodgate-300-10F"),
            Some(ImportedClock::Fischer {
                total_sec: 300,
                increment_sec: 10
            })
        );
        assert_eq!(
            clock_from_wdoor_game_name("floodgate-600-10"),
            Some(ImportedClock::Countdown {
                total_sec: 600,
                byoyomi_sec: 10
            })
        );
        assert_eq!(
            clock_from_wdoor_game_name("my-event-900-0"),
            Some(ImportedClock::Countdown {
                total_sec: 900,
                byoyomi_sec: 0
            })
        );
        for unknown in [
            "floodgate",
            "floodgate-300",
            "300-10F",
            "a-x-10",
            "a-300-F",
            "a-300-10f",
            "a-300-+5",
            "a--10",
        ] {
            assert_eq!(clock_from_wdoor_game_name(unknown), None, "name={unknown}");
        }
    }

    #[test]
    fn header_clock_uses_time_limit_and_increment() {
        assert_eq!(
            clock_from_headers(Some("00:15+05"), None),
            Some(ImportedClock::Countdown {
                total_sec: 900,
                byoyomi_sec: 5
            })
        );
        assert_eq!(
            clock_from_headers(Some("01:30+00"), Some("0")),
            Some(ImportedClock::Countdown {
                total_sec: 5400,
                byoyomi_sec: 0
            })
        );
        assert_eq!(
            clock_from_headers(Some("00:05+00"), Some("10")),
            Some(ImportedClock::Fischer {
                total_sec: 300,
                increment_sec: 10
            })
        );
        // 持ち時間本体が分からなければ省略する。
        assert_eq!(clock_from_headers(None, Some("10")), None);
        assert_eq!(clock_from_headers(None, None), None);
        for bad in ["", "15", "00:15", "aa:15+05", "00:15+x", "99999999:00+00"] {
            assert_eq!(clock_from_headers(Some(bad), None), None, "bad={bad:?}");
        }
    }

    #[test]
    fn clock_wire_shape_matches_server_clock_spec() {
        let fischer = serde_json::to_value(
            ImportedClock::Fischer {
                total_sec: 300,
                increment_sec: 10,
            }
            .to_wire(),
        )
        .unwrap();
        assert_eq!(
            fischer,
            serde_json::to_value(ClockSpec::from_server(&rshogi_csa_server::ClockSpec::Fischer {
                total_time_sec: 300,
                increment_sec: 10,
            }))
            .unwrap()
        );
        let countdown = serde_json::to_value(
            ImportedClock::Countdown {
                total_sec: 600,
                byoyomi_sec: 10,
            }
            .to_wire(),
        )
        .unwrap();
        assert_eq!(
            countdown,
            serde_json::to_value(ClockSpec::from_server(
                &rshogi_csa_server::ClockSpec::Countdown {
                    total_time_sec: 600,
                    byoyomi_sec: 10,
                }
            ))
            .unwrap()
        );
    }

    // ---- 結果 ----

    #[test]
    fn summary_reasons_follow_shogi_server_vocabulary() {
        for (summary, expected) in [
            ("toryo:RAMU win:Panda lose", ("WIN_BLACK", "RESIGN")),
            ("toryo:RAMU lose:Panda win", ("WIN_WHITE", "RESIGN")),
            ("time up:a lose:b win", ("WIN_WHITE", "TIME_UP")),
            ("illegal move:a win:b lose", ("WIN_BLACK", "ILLEGAL")),
            ("uchifuzume:a lose:b win", ("WIN_WHITE", "ILLEGAL")),
            ("oute_kaihimore:a lose:b win", ("WIN_WHITE", "ILLEGAL")),
            ("outori:a win:b lose", ("WIN_BLACK", "ILLEGAL")),
            ("illegal kachi:a lose:b win", ("WIN_WHITE", "ILLEGAL")),
            ("kachi:a win:b lose", ("WIN_BLACK", "JISHOGI")),
            ("oute_sennichite:a lose:b win", ("WIN_WHITE", "OUTE_SENNICHITE")),
            ("sennichite:a draw:b draw", ("DRAW", "SENNICHITE")),
            ("max_moves:a draw:b draw", ("DRAW", "MAX_MOVES")),
            ("abnormal:a win:b lose", ("WIN_BLACK", "ABNORMAL")),
            ("abnormal:a lose:b win", ("WIN_WHITE", "ABNORMAL")),
        ] {
            assert_eq!(result_from_summary(summary), expected, "summary={summary}");
        }
    }

    #[test]
    fn summary_ignores_player_names_and_degrades_safely() {
        // レート付き対局者は `<名前>+<hash>` で書かれうる。名前に空白があっても末尾語だけ見る。
        assert_eq!(
            result_from_summary("toryo:RAMU+c902d756 win:Panda+05db2b40 lose"),
            ("WIN_BLACK", "RESIGN")
        );
        assert_eq!(result_from_summary("toryo:a b win:c d lose"), ("WIN_BLACK", "RESIGN"));
        // 未知の理由は ABNORMAL。勝敗語は読めれば活かす。
        assert_eq!(result_from_summary("mystery:a win:b lose"), ("WIN_BLACK", "ABNORMAL"));
        assert_eq!(result_from_summary("TORYO:a win:b lose"), ("WIN_BLACK", "ABNORMAL"));
        // 勝敗語が読めない / 矛盾する場合は ABORT。
        for broken in [
            "toryo",
            "toryo:",
            "toryo:a win",
            "toryo:a win:b win",
            "toryo:a won:b lost",
            "",
        ] {
            assert_eq!(result_from_summary(broken).0, "ABORT", "summary={broken:?}");
        }
    }

    #[test]
    fn special_commands_resolve_winner_from_side_to_move() {
        for (special, black_to_move, expected) in [
            ("TORYO", true, ("WIN_WHITE", "RESIGN")),
            ("TORYO", false, ("WIN_BLACK", "RESIGN")),
            ("TIME_UP", true, ("WIN_WHITE", "TIME_UP")),
            ("TIME_UP", false, ("WIN_BLACK", "TIME_UP")),
            ("ILLEGAL_MOVE", true, ("WIN_BLACK", "ILLEGAL")),
            ("ILLEGAL_MOVE", false, ("WIN_WHITE", "ILLEGAL")),
            ("+ILLEGAL_ACTION", false, ("WIN_WHITE", "ILLEGAL")),
            ("-ILLEGAL_ACTION", true, ("WIN_BLACK", "ILLEGAL")),
            ("KACHI", true, ("WIN_BLACK", "JISHOGI")),
            ("KACHI", false, ("WIN_WHITE", "JISHOGI")),
            ("JISHOGI", true, ("DRAW", "JISHOGI")),
            ("SENNICHITE", true, ("DRAW", "SENNICHITE")),
            ("MAX_MOVES", false, ("DRAW", "MAX_MOVES")),
            ("HIKIWAKE", true, ("DRAW", "ABNORMAL")),
            ("CHUDAN", true, ("ABORT", "ABNORMAL")),
            ("UNKNOWN", true, ("ABORT", "ABNORMAL")),
        ] {
            assert_eq!(
                result_from_special(Some(special), black_to_move),
                expected,
                "special={special} black_to_move={black_to_move}"
            );
        }
        assert_eq!(result_from_special(None, true), ("ABORT", "ABNORMAL"));
    }

    #[test]
    fn every_derived_end_reason_is_one_of_the_existing_eight() {
        const END_REASONS: [&str; 8] = [
            "RESIGN",
            "TIME_UP",
            "ILLEGAL",
            "JISHOGI",
            "OUTE_SENNICHITE",
            "SENNICHITE",
            "MAX_MOVES",
            "ABNORMAL",
        ];
        const RESULT_KINDS: [&str; 4] = ["WIN_BLACK", "WIN_WHITE", "DRAW", "ABORT"];
        let summaries = [
            "toryo",
            "time up",
            "illegal move",
            "uchifuzume",
            "oute_kaihimore",
            "outori",
            "illegal kachi",
            "kachi",
            "oute_sennichite",
            "sennichite",
            "max_moves",
            "abnormal",
            "x",
        ];
        for reason in summaries {
            let (kind, end_reason) = result_from_summary(&format!("{reason}:a win:b lose"));
            assert!(END_REASONS.contains(&end_reason) && RESULT_KINDS.contains(&kind));
        }
        for special in [
            "TORYO",
            "TIME_UP",
            "ILLEGAL_MOVE",
            "+ILLEGAL_ACTION",
            "-ILLEGAL_ACTION",
            "KACHI",
            "JISHOGI",
            "SENNICHITE",
            "MAX_MOVES",
            "HIKIWAKE",
            "CHUDAN",
            "TSUMI",
        ] {
            let (kind, end_reason) = result_from_special(Some(special), true);
            assert!(END_REASONS.contains(&end_reason) && RESULT_KINDS.contains(&kind));
        }
    }

    // ---- ヘッダ走査 ----

    #[test]
    fn scan_headers_reads_wdoor_and_plain_forms() {
        let csa = wdoor_csa();
        let headers = scan_headers(&csa);
        assert_eq!(headers.event, Some(WDOOR_ID));
        assert_eq!(headers.start_time, Some("2026/10/09 00:00:00"));
        assert_eq!(headers.end_time, Some("2026/10/09 00:21:55"));
        assert_eq!(headers.summary, Some("toryo:RAMU lose:Panda win"));
        assert_eq!(headers.increment, Some("10"));
        assert_eq!(headers.last_special, Some("TORYO"));

        let headers = scan_headers("$END_TIME:2026/05/03 11:00:00\r\n%TIME_UP,T12\r\n");
        assert_eq!(headers.end_time, Some("2026/05/03 11:00:00"));
        assert_eq!(headers.last_special, Some("TIME_UP"));
        assert_eq!(scan_headers("+7776FU\n"), CsaHeaders::default());
    }

    // ---- 取り込み全体 ----

    #[test]
    fn import_wdoor_derives_full_meta() {
        let csa = wdoor_csa();
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        let game = import_wdoor(&id, &csa, Some(id.kifu_url()), true).unwrap();
        assert_eq!(
            game,
            ImportedGame {
                game_id: "wdoor-20261009000001-93ef4d36a6".to_owned(),
                started_at_ms: utc_ms(2026, 10, 8, 15, 0, 0),
                ended_at_ms: utc_ms(2026, 10, 8, 15, 21, 55),
                black: "RAMU".to_owned(),
                white: "Panda".to_owned(),
                result_kind: "WIN_WHITE",
                end_reason: "RESIGN",
                moves_count: 4,
                clock: Some(ImportedClock::Fischer {
                    total_sec: 300,
                    increment_sec: 10
                }),
                source: "wdoor",
                event: Some("floodgate-300-10F".to_owned()),
                origin_url: Some(id.kifu_url()),
            }
        );
    }

    #[test]
    fn imported_wdoor_entry_matches_wire_contract() {
        let csa = wdoor_csa();
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        let game = import_wdoor(&id, &csa, Some(id.kifu_url()), true).unwrap();
        assert_eq!(
            serde_json::to_value(game.entry()).unwrap(),
            serde_json::json!({
                "game_id": "wdoor-20261009000001-93ef4d36a6",
                "started_at_ms": utc_ms(2026, 10, 8, 15, 0, 0),
                "ended_at_ms": utc_ms(2026, 10, 8, 15, 21, 55),
                "black_handle": "RAMU",
                "white_handle": "Panda",
                "result_kind": "WIN_WHITE",
                "end_reason": "RESIGN",
                "moves_count": 4,
                "clock": {"kind": "fischer", "total_sec": 300, "increment_sec": 10},
                "source": "wdoor",
                "event": "floodgate-300-10F",
                "origin_url": "https://wdoor.c.u-tokyo.ac.jp/shogi/x/2026/10/09/wdoor+floodgate-300-10F+RAMU+Panda+20261009000001.csa",
            })
        );
    }

    #[test]
    fn import_wdoor_rejects_game_in_progress() {
        let csa = wdoor_csa();
        let in_progress = csa.split("%TORYO").next().unwrap();
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        assert_eq!(import_wdoor(&id, in_progress, None, true), Err(ImportError::Unfinished));
        // 未終局判定は再生より先 (本文が HTML などでも 409 側に倒れる)。
        assert_eq!(
            import_wdoor(&id, "<html>busy</html>", None, true),
            Err(ImportError::Unfinished)
        );
    }

    #[test]
    fn summary_wins_over_trailing_special_command() {
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        let base = wdoor_csa();
        // `abnormal` の棋譜は末尾が `%TORYO` のことがある。4 手後の先手番で `%TORYO` なら
        // `%` だけ見ると後手勝ちだが、summary が先手勝ちならそちらを採る。
        let abnormal = base
            .replace("'summary:toryo:RAMU lose:Panda win", "'summary:abnormal:RAMU win:Panda lose");
        let game = import_wdoor(&id, &abnormal, None, true).unwrap();
        assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "ABNORMAL"));

        // 末尾に `%` コマンドが無い棋譜 (oute_sennichite / illegal move / max_moves /
        // oute_kaihimore) も summary だけで結果が決まる。
        for (summary, expected) in [
            ("oute_sennichite:RAMU lose:Panda win", ("WIN_WHITE", "OUTE_SENNICHITE")),
            ("illegal move:RAMU win:Panda lose", ("WIN_BLACK", "ILLEGAL")),
            ("oute_kaihimore:RAMU lose:Panda win", ("WIN_WHITE", "ILLEGAL")),
            ("max_moves:RAMU draw:Panda draw", ("DRAW", "MAX_MOVES")),
            ("sennichite:RAMU draw:Panda draw", ("DRAW", "SENNICHITE")),
        ] {
            let csa = base
                .replace(
                    "%TORYO
", "",
                )
                .replace("toryo:RAMU lose:Panda win", summary);
            assert!(!csa.contains('%'), "fixture must not keep a special command");
            let game = import_wdoor(&id, &csa, None, true).unwrap();
            assert_eq!((game.result_kind, game.end_reason), expected, "summary={summary}");
            assert_eq!(game.moves_count, 4);
        }
    }

    #[test]
    fn kachi_line_with_inline_comment_is_handled() {
        // wdoor の入玉宣言は `%KACHI,'* 100000 ` のようにコメントが連結された行になる。
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        let csa = wdoor_csa()
            .replace(
                "%TORYO
",
                "%KACHI,'* 100000 
",
            )
            .replace("toryo:RAMU lose:Panda win", "kachi:RAMU win:Panda lose");
        assert_eq!(scan_headers(&csa).last_special, Some("KACHI"));
        let game = import_wdoor(&id, &csa, None, true).unwrap();
        assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "JISHOGI"));
        assert_eq!(game.moves_count, 4);
        // summary の無いアーカイブでも `%KACHI` の手番側 (4 手後の先手) の勝ちになる。
        let without_summary = csa.replace(
            "'summary:kachi:RAMU win:Panda lose
",
            "",
        );
        let game = import_wdoor_archive(&without_summary).unwrap();
        assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "JISHOGI"));
    }

    #[test]
    fn import_wdoor_falls_back_to_id_time_and_elapsed_sum() {
        let csa = wdoor_csa()
            .replace("$START_TIME:2026/10/09 00:00:00\n", "")
            .replace("'$END_TIME:2026/10/09 00:21:55\n", "");
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        let game = import_wdoor(&id, &csa, None, true).unwrap();
        // 開始は ID の 00:00:01 JST、終了は開始 + (0 + 10 + 0 + 14) 秒。
        assert_eq!(game.started_at_ms, utc_ms(2026, 10, 8, 15, 0, 1));
        assert_eq!(game.ended_at_ms, utc_ms(2026, 10, 8, 15, 0, 25));
    }

    #[test]
    fn import_wdoor_archive_uses_event_id_and_omits_origin_url() {
        let csa = wdoor_csa();
        let game = import_wdoor_archive(&csa).unwrap();
        let id = WdoorGameId::parse(WDOOR_ID).unwrap();
        // 参照取り込みと同じ game_id になる。
        assert_eq!(game.game_id, id.game_id());
        assert_eq!(game.source, "wdoor");
        assert_eq!(game.event.as_deref(), Some("floodgate-300-10F"));
        assert_eq!(game.origin_url, None);

        // summary の無い古い棋譜でも `%` から結果を決めて取り込める。
        let without_summary = csa.replace("'summary:toryo:RAMU lose:Panda win\n", "");
        let game = import_wdoor_archive(&without_summary).unwrap();
        assert_eq!((game.result_kind, game.end_reason), ("WIN_WHITE", "RESIGN"));
    }

    #[test]
    fn import_wdoor_archive_requires_wdoor_event() {
        assert_eq!(import_wdoor_archive(&upload_csa("%TORYO\n")), Err(ImportError::NotWdoorEvent));
        let no_event = wdoor_csa().replace(&format!("$EVENT:{WDOOR_ID}\n"), "");
        assert_eq!(import_wdoor_archive(&no_event), Err(ImportError::NotWdoorEvent));
    }

    #[test]
    fn import_upload_derives_meta_from_headers_and_special() {
        let csa = upload_csa("%TORYO\n");
        let game = import_upload(&csa, Some("WCSC36")).unwrap();
        assert_eq!(game.game_id, upload_game_id(csa.as_bytes()));
        assert!(game.game_id.starts_with("upload-") && game.game_id.len() == "upload-".len() + 16);
        assert_eq!(game.source, "upload");
        assert_eq!(game.event.as_deref(), Some("WCSC36"));
        assert_eq!(game.origin_url, None);
        assert_eq!((game.black.as_str(), game.white.as_str()), ("alice", "bob"));
        assert_eq!(game.moves_count, 2);
        // 2 手指して先手番で投了 → 後手勝ち。
        assert_eq!((game.result_kind, game.end_reason), ("WIN_WHITE", "RESIGN"));
        assert_eq!(game.started_at_ms, utc_ms(2026, 5, 3, 1, 0, 0));
        // `$END_TIME` が無いので開始 + 3 + 4 秒。
        assert_eq!(game.ended_at_ms, game.started_at_ms + 7_000);
        assert_eq!(
            game.clock,
            Some(ImportedClock::Countdown {
                total_sec: 900,
                byoyomi_sec: 5
            })
        );
        let entry = serde_json::to_value(game.entry()).unwrap();
        let object = entry.as_object().unwrap();
        for absent in ["black_player_id", "white_player_id", "origin_url"] {
            assert!(!object.contains_key(absent), "{absent} must be omitted: {entry}");
        }
    }

    #[test]
    fn import_upload_without_event_or_clock_omits_them() {
        let csa = upload_csa("%CHUDAN\n").replace("$TIME_LIMIT:00:15+05\n", "");
        let game = import_upload(&csa, None).unwrap();
        assert_eq!((game.result_kind, game.end_reason), ("ABORT", "ABNORMAL"));
        let entry = serde_json::to_value(game.entry()).unwrap();
        let object = entry.as_object().unwrap();
        assert!(!object.contains_key("event") && !object.contains_key("clock"), "{entry}");
        // 終局コマンドの無い棋譜も ABORT として取り込む (upload に未終局 409 は無い)。
        let game = import_upload(&upload_csa(""), None).unwrap();
        assert_eq!((game.result_kind, game.end_reason), ("ABORT", "ABNORMAL"));
    }

    #[test]
    fn import_upload_prefers_summary_and_end_time_when_present() {
        let csa = upload_csa(
            "%TORYO\n'summary:time up:alice win:bob lose\n$END_TIME:2026/05/03 10:30:00\n",
        );
        let game = import_upload(&csa, None).unwrap();
        assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "TIME_UP"));
        assert_eq!(game.ended_at_ms, utc_ms(2026, 5, 3, 1, 30, 0));
    }

    #[test]
    fn end_time_before_start_is_clamped_to_start() {
        let csa = upload_csa("%TORYO\n$END_TIME:2026/05/03 09:00:00\n");
        let game = import_upload(&csa, None).unwrap();
        assert_eq!(game.ended_at_ms, game.started_at_ms);
    }

    #[test]
    fn import_rejects_kifu_that_cannot_be_replayed() {
        for (label, csa) in [
            ("illegal move", upload_csa("+7775FU,T1\n-3435FU,T1\n+2828HI,T1\n%TORYO\n")),
            ("wrong turn", upload_csa("-8384FU,T1\n%TORYO\n")),
            ("no piece at source", upload_csa("+5555FU,T1\n%TORYO\n")),
            (
                "drop without piece in hand",
                upload_csa(
                    "+7675FU,T1\n-3435FU,T1\n+7574FU,T1\n-7374FU,T1\n+0076FU,T1\n-0073FU,T1\n+0075FU,T1\n",
                ),
            ),
            (
                "missing names",
                "V2.2\n$START_TIME:2026/05/03 10:00:00\nPI\n+\n+7776FU\n%TORYO\n".to_owned(),
            ),
            (
                "missing start time",
                upload_csa("%TORYO\n").replace("$START_TIME:2026/05/03 10:00:00\n", ""),
            ),
            (
                "broken board",
                "V2.2\nN+a\nN-b\n$START_TIME:2026/05/03 10:00:00\nP1 * \n+\n".to_owned(),
            ),
        ] {
            assert!(
                matches!(import_upload(&csa, None), Err(ImportError::Unplayable(_))),
                "{label}: {:?}",
                import_upload(&csa, None)
            );
        }
    }

    #[test]
    fn trailing_illegal_move_is_accepted_only_with_illegal_move_command() {
        // 先手が 3 手目に動けない手 (飛車がその場に移動) を指し、`%ILLEGAL_MOVE` で終わる。
        let csa = upload_csa("+2828HI,T1\n%ILLEGAL_MOVE\n");
        let game = import_upload(&csa, None).unwrap();
        assert_eq!(game.moves_count, 3);
        // 反則手を指したのは先手 → 後手勝ち。
        assert_eq!((game.result_kind, game.end_reason), ("WIN_WHITE", "ILLEGAL"));
        // 途中の反則手や、別の終局コマンドでは許さない。
        for tail in [
            "+2828HI,T1\n-8384FU,T1\n%ILLEGAL_MOVE\n",
            "+2828HI,T1\n%TORYO\n",
            "+2828HI,T1\n",
        ] {
            assert!(
                matches!(import_upload(&upload_csa(tail), None), Err(ImportError::Unplayable(_))),
                "tail={tail:?}"
            );
        }
        // summary がある (shogi-server 形式の) 棋譜は反則手を指し手に含めないので許さない。
        let with_summary =
            upload_csa("+2828HI,T1\n%ILLEGAL_MOVE\n'summary:illegal move:alice lose:bob win\n");
        assert!(matches!(import_upload(&with_summary, None), Err(ImportError::Unplayable(_))));
    }

    #[test]
    fn replay_supports_handicap_style_initial_positions_and_white_first() {
        // 後手番から始まる局面 (P 行 + `-`)。
        let csa = format!(
            "V2.2\nN+a\nN-b\n$START_TIME:2026/05/03 10:00:00\n{}-3334FU,T1\n+7776FU,T1\n%TORYO\n",
            BOARD.replace("\n+\n", "\n-\n")
        );
        let game = import_upload(&csa, None).unwrap();
        assert_eq!(game.moves_count, 2);
        // 後手番で投了 → 先手勝ち。
        assert_eq!((game.result_kind, game.end_reason), ("WIN_BLACK", "RESIGN"));
    }

    #[test]
    fn normalize_event_trims_and_bounds() {
        assert_eq!(normalize_event(None), Ok(None));
        assert_eq!(normalize_event(Some("")), Ok(None));
        assert_eq!(normalize_event(Some("   ")), Ok(None));
        assert_eq!(normalize_event(Some(" WCSC36 ")), Ok(Some("WCSC36")));
        assert_eq!(
            normalize_event(Some("第36回 世界コンピュータ将棋選手権")),
            Ok(Some("第36回 世界コンピュータ将棋選手権"))
        );
        let at_limit = "あ".repeat(MAX_EVENT_CHARS);
        assert_eq!(normalize_event(Some(&at_limit)), Ok(Some(at_limit.as_str())));
        assert!(normalize_event(Some(&"あ".repeat(MAX_EVENT_CHARS + 1))).is_err());
        assert!(normalize_event(Some("a\nb")).is_err());
    }
}
