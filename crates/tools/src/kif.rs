//! Selfplay/tournament 形式の JSONL 対局ログを KIF 棋譜に変換するライブラリ。
//!
//! `meta` / `move` / `result` の 3 種の JSONL レコードを受け取り、対局ごとに
//! KIF (柿木将棋形式) を出力する。`tournament` バイナリの出力をはじめ、
//! 同形式の move ログを含む jsonl 全般に対応する。
//!
//! `gensfen` の出力は move 行を含まないため変換対象にはならない。

use rshogi_csa_client::jsonl::{is_csa_jsonl, is_incomplete_result};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use rshogi_core::movegen::is_legal_with_pass;
use rshogi_core::position::Position;
use rshogi_core::types::{Color, File as BoardFile, Move, PieceType, Rank, Square};
use serde::Deserialize;
use serde_json::Value;

use crate::output_path::ensure_safe_output_path;
use crate::selfplay::EvalLog;
use crate::selfplay::position::is_legal_game_move;

/// 変換対象を絞り込むフィルタ。
#[derive(Default, Debug, Clone)]
pub struct GameFilter {
    /// 抽出対象の game_id。空なら全対局。
    pub game_ids: Vec<u32>,
    /// 先頭から N 局スキップ。フィルタ適用後に評価。
    pub skip: usize,
    /// 出力する対局数の上限。`None` で無制限。
    pub limit: Option<usize>,
}

impl GameFilter {
    pub fn matches_id(&self, id: u32) -> bool {
        self.game_ids.is_empty() || self.game_ids.contains(&id)
    }
}

/// 入力 jsonl をパースし、対局ごとに KIF を書き出す。
///
/// `output` がディレクトリなら `<output>/g<game_id:03>.kif` に出力する。
/// `output` がファイルパスで、対象対局が複数の場合は
/// `<stem>_g<game_id:03>.<ext>` の連番に展開する（単一対局なら `output` 自体に書く）。
///
/// 戻り値は実際に書き出したファイルパスのリスト。
pub fn convert_jsonl_to_kif(
    input: &Path,
    output: &Path,
    filter: &GameFilter,
) -> Result<Vec<PathBuf>> {
    let file =
        File::open(input).with_context(|| format!("failed to open input {}", input.display()))?;
    let reader = BufReader::new(file);

    let mut meta: Option<KifMeta> = None;
    let mut games: BTreeMap<u32, GameLog> = BTreeMap::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("failed to parse JSON line: {}", trimmed))?;
        match value.get("type").and_then(|v| v.as_str()) {
            Some("meta") => match serde_json::from_value(value) {
                Ok(m) => meta = Some(m),
                Err(e) => eprintln!("warning: failed to parse meta line: {}", e),
            },
            Some("move") => {
                let entry: MoveEntry = serde_json::from_value(value)?;
                games.entry(entry.game_id).or_default().moves.push(entry);
            }
            Some("result") => {
                let game_id = value
                    .get("game_id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| anyhow!("result entry missing game_id"))?
                    as u32;
                let entry: ResultEntry = serde_json::from_value(value)?;
                games.entry(game_id).or_default().result = Some(entry);
            }
            _ => {}
        }
    }

    if games.is_empty() {
        bail!("no games found in {}", input.display());
    }

    // フィルタ適用: id 指定 → skip → limit
    let selected: Vec<(u32, GameLog)> = games
        .into_iter()
        .filter(|(id, _)| filter.matches_id(*id))
        .skip(filter.skip)
        .take(filter.limit.unwrap_or(usize::MAX))
        .collect();

    if selected.is_empty() {
        bail!(
            "no games matched filter (game_ids={:?}, skip={}, limit={:?})",
            filter.game_ids,
            filter.skip,
            filter.limit
        );
    }

    // 既存ディレクトリ、または拡張子のないパス（未存在ディレクトリ想定）を
    // ディレクトリ扱いする。拡張子なし単一ファイルを出力したい場合はこの判定で
    // ディレクトリ扱いになるので、ファイルとして書きたいときは拡張子を付ける。
    let output_is_dir =
        output.is_dir() || (output.extension().is_none() && !output.as_os_str().is_empty());

    let single = selected.len() == 1 && !output_is_dir;
    let mut paths = Vec::new();

    if output_is_dir {
        std::fs::create_dir_all(output)
            .with_context(|| format!("failed to create dir {}", output.display()))?;
    }

    for (game_id, _) in &selected {
        let path = if output_is_dir {
            output.join(format!("g{game_id:03}.kif"))
        } else if single {
            output.to_path_buf()
        } else {
            let parent = output.parent().unwrap_or_else(|| Path::new("."));
            let stem = output.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
            let ext = output.extension().and_then(|s| s.to_str()).unwrap_or("kif");
            parent.join(format!("{stem}_g{game_id:03}.{ext}"))
        };
        paths.push(path);
    }
    for path in &paths {
        ensure_safe_output_path(path, input)?;
    }
    for ((game_id, game), path) in selected.iter().zip(&paths) {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut writer = BufWriter::new(staged.as_file_mut());
            export_game_to_kif(&mut writer, meta.as_ref(), *game_id, game)?;
            writer.flush()?;
        }
        ensure_safe_output_path(path, input)?;
        staged
            .persist(path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
    }
    Ok(paths)
}

#[derive(Default)]
struct GameLog {
    moves: Vec<MoveEntry>,
    result: Option<ResultEntry>,
}

#[derive(Deserialize, Clone)]
struct MoveEntry {
    game_id: u32,
    ply: u32,
    sfen_before: String,
    move_usi: String,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    timed_out: bool,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    eval: Option<EvalLog>,
}

#[derive(Deserialize)]
struct ResultEntry {
    outcome: String,
    reason: String,
    #[serde(default)]
    pair_slot: Option<u32>,
}

/// KIF 生成に必要な meta フィールドのみのサブセット。
#[derive(Deserialize, Default)]
struct KifMeta {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    settings: KifMetaSettings,
    #[serde(default)]
    engine_cmd: KifMetaEngineCmd,
}

#[derive(Deserialize, Default)]
struct KifMetaSettings {
    #[serde(default)]
    btime: u64,
    #[serde(default)]
    wtime: Option<u64>,
    #[serde(default)]
    binc: u64,
    #[serde(default)]
    winc: Option<u64>,
}

impl KifMetaSettings {
    fn white_time(&self) -> u64 {
        self.wtime.unwrap_or(self.btime)
    }

    fn white_increment(&self) -> u64 {
        self.winc.unwrap_or(self.binc)
    }
}

#[derive(Deserialize, Default)]
struct KifMetaEngineCmd {
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    path_black: String,
    #[serde(default)]
    label_black: Option<String>,
    #[serde(default)]
    path_white: String,
    #[serde(default)]
    label_white: Option<String>,
    #[serde(default)]
    usi_options_black: Vec<String>,
    #[serde(default)]
    usi_options_white: Vec<String>,
}

fn export_game_to_kif<W: Write>(
    writer: &mut W,
    meta: Option<&KifMeta>,
    game_id: u32,
    game: &GameLog,
) -> Result<()> {
    let mut pos = start_position_for_game(game_id, &game.moves)?;

    let timestamp = meta.map(|m| m.timestamp.clone()).unwrap_or_else(|| "-".to_string());
    let (mut black_name, mut white_name) = engine_names_for(meta);
    let meta_names = (black_name.clone(), white_name.clone());
    if game.result.as_ref().and_then(|result| result.pair_slot) == Some(1) {
        std::mem::swap(&mut black_name, &mut white_name);
    }
    let display_engine = |engine: &str| {
        let Some(meta) = meta else {
            return engine.to_owned();
        };
        if meta.engine_cmd.label_black.as_deref() == Some(engine) {
            return meta_names.0.clone();
        }
        if meta.engine_cmd.label_white.as_deref() == Some(engine) {
            return meta_names.1.clone();
        }
        let black_matches = Path::new(&meta.engine_cmd.path_black)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(engine);
        let white_matches = Path::new(&meta.engine_cmd.path_white)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(engine);
        match (black_matches, white_matches) {
            (true, false) => meta_names.0.clone(),
            (false, true) => meta_names.1.clone(),
            _ => engine.to_owned(),
        }
    };
    let mut found_black = false;
    let mut found_white = false;
    for entry in &game.moves {
        let Some(engine) = entry.engine.as_ref() else {
            continue;
        };
        let mut before = Position::new();
        before
            .set_sfen(&entry.sfen_before)
            .map_err(|error| anyhow!("invalid sfen_before: {error}"))?;
        if before.side_to_move() == Color::Black && !found_black {
            black_name = display_engine(engine);
            found_black = true;
        } else if before.side_to_move() == Color::White && !found_white {
            white_name = display_engine(engine);
            found_white = true;
        }
        if found_black && found_white {
            break;
        }
    }
    let (btime, wtime) =
        meta.map(|m| (m.settings.btime, m.settings.white_time())).unwrap_or((0, 0));
    writeln!(writer, "開始日時：{}", format_kif_datetime(&timestamp))?;
    if is_hirate(&pos) {
        writeln!(writer, "手合割：平手")?;
    }
    writeln!(writer, "先手：{}", black_name)?;
    writeln!(writer, "後手：{}", white_name)?;
    writeln!(writer, "持ち時間：先手{}ms / 後手{}ms", btime, wtime)?;
    if let Some(settings) = meta.map(|m| &m.settings)
        && (settings.binc > 0 || settings.white_increment() > 0)
    {
        writeln!(
            writer,
            "加算時間：先手{}ms / 後手{}ms",
            settings.binc,
            settings.white_increment()
        )?;
    }
    if !is_hirate(&pos) {
        write_bod(writer, &pos)?;
    }
    writeln!(writer, "手数----指手---------消費時間--")?;

    let mut moves = game.moves.clone();
    moves.sort_by_key(|m| m.ply);
    let mut total_black = 0u64;
    let mut total_white = 0u64;
    let mut written_moves = 0u32;

    for entry in moves {
        if entry.timed_out
            || matches!(entry.move_usi.as_str(), "resign" | "win" | "timeout" | "illegal" | "none")
        {
            break;
        }
        let side = pos.side_to_move();
        let mv = Move::from_usi(&entry.move_usi)
            .ok_or_else(|| anyhow!("invalid move in log: {}", entry.move_usi))?;
        if !is_legal_with_pass(&pos, mv) {
            bail!("illegal move '{}' in log for game {}", entry.move_usi, game_id);
        }
        let elapsed_ms = entry.elapsed_ms.unwrap_or(0);
        let total_time = if side == Color::Black {
            total_black + elapsed_ms
        } else {
            total_white + elapsed_ms
        };
        written_moves += 1;
        let line = format_move_kif(written_moves, &pos, mv, elapsed_ms, total_time);
        writeln!(writer, "{line}")?;
        let gives_check = if mv.is_pass() {
            false
        } else {
            pos.gives_check(mv)
        };
        pos.do_move(mv, gives_check);
        if side == Color::Black {
            total_black = total_time;
        } else {
            total_white = total_time;
        }
        // 評価値・詰みの向きは指した側（指す前の手番）で決まる。読み筋は指した後の局面から書く。
        for comment in eval_comment_lines(&pos, side, mv, entry.eval.as_ref()) {
            writeln!(writer, "{comment}")?;
        }
    }

    let csa = meta.is_some_and(|m| is_csa_jsonl(m.source.as_deref(), m.engine_cmd.note.as_deref()));
    let result = game
        .result
        .as_ref()
        .filter(|r| !is_incomplete_result(csa, &r.outcome, Some(&r.reason)));
    let terminal = match result {
        // 打ち切り（selfplay の cancelled / game_time_limit / watchdog）は勝敗が付いていない。
        Some(r) if r.outcome == "in_progress" => Some("中断"),
        Some(r) => terminal_move_label(&r.reason, &r.outcome, pos.side_to_move(), csa),
        None if game.result.is_some() => Some("中断"),
        None => None,
    };
    if let Some(label) = terminal {
        writeln!(writer, "{:>4} {}", written_moves + 1, label)?;
    }
    if let Some(res) = game.result.as_ref()
        && res.reason != "max_moves"
    {
        writeln!(writer, "*終了理由={}", res.reason)?;
    }
    let summary = match (result.map(|r| r.outcome.as_str()), terminal) {
        (Some("black_win"), _) => "先手の勝ち",
        (Some("white_win"), _) => "後手の勝ち",
        (Some("draw"), Some(label @ ("千日手" | "持将棋"))) => label,
        (Some("draw"), _) => "引き分け",
        _ => "中断",
    };
    writeln!(writer, "\nまで{}手で{}", written_moves, summary)?;
    Ok(())
}

/// 終局の行（`201 投了` など）に書く語。KIF の終局の語は「次に指す番の側」から見た意味を持つ
/// （投了・切れ負け・反則負けは手番側の負け、入玉勝ち・反則勝ちは手番側の勝ち）ので、
/// 記録された勝敗と食い違うとき、対応する語が無いときは None（`*終了理由=` のコメントだけ残す）。
fn terminal_move_label(
    reason: &str,
    outcome: &str,
    side_to_move: Color,
    csa: bool,
) -> Option<&'static str> {
    enum Expect {
        MoverLoses,
        MoverWins,
        Draw,
    }
    let winner = match outcome {
        "black_win" => Some(Color::Black),
        "white_win" => Some(Color::White),
        "draw" => None,
        _ => return None,
    };
    let (label, expect) = match reason {
        "resign" => ("投了", Expect::MoverLoses),
        // csa_client は理由行の無い `#WIN` / `#LOSE` を `win` / `lose` と書く（投了などの通常の決着）。
        "win" | "lose" if csa => ("投了", Expect::MoverLoses),
        // selfplay の `win` はエンジンの入玉宣言（`bestmove win`）。
        "win" | "win_declaration" => ("入玉勝ち", Expect::MoverWins),
        "timeout" | "time_up" => ("切れ負け", Expect::MoverLoses),
        "illegal_move" => ("反則負け", Expect::MoverLoses),
        // 連続王手の千日手は王手を掛け続けた側の負け。循環を閉じたのが王手か逃げる手かで、
        // 次に指す番の側が勝つことも負けることもある。
        "sennichite_perpetual_check" | "oute_sennichite" if winner == Some(side_to_move) => {
            ("反則勝ち", Expect::MoverWins)
        }
        "sennichite_perpetual_check" | "oute_sennichite" => ("反則負け", Expect::MoverLoses),
        "sennichite" => ("千日手", Expect::Draw),
        // CSA の旧ログの `max_moves` は引き分けか打ち切りか決められないので、語を書かない。
        "max_moves" if !csa => ("持将棋", Expect::Draw),
        "jishogi" => ("持将棋", Expect::Draw),
        _ => return None,
    };
    let consistent = match expect {
        Expect::MoverLoses => winner == Some(!side_to_move),
        Expect::MoverWins => winner == Some(side_to_move),
        Expect::Draw => winner.is_none(),
    };
    consistent.then_some(label)
}

/// RFC 3339 の時刻を KIF の慣例 `YYYY/MM/DD HH:MM:SS`（時刻に付いていた時差のまま）にする。
/// 読めなければそのまま返す。
fn format_kif_datetime(timestamp: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|t| t.format("%Y/%m/%d %H:%M:%S").to_string())
        .unwrap_or_else(|_| timestamp.to_owned())
}

/// 平手の初期配置・持ち駒なし・先手番か（手数は見ない）。
fn is_hirate(pos: &Position) -> bool {
    let sfen = pos.to_sfen();
    let fields: Vec<&str> = sfen.split_whitespace().take(3).collect();
    fields
        == [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL",
            "b",
            "-",
        ]
}

/// 開始局面を KIF の局面図（BOD）で書く。平手以外の開始局面は、KIF ではこの形でしか表せない。
fn write_bod<W: Write>(writer: &mut W, pos: &Position) -> Result<()> {
    const RANKS: [&str; 9] = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];
    writeln!(writer, "後手の持駒：{}", format_hand(pos, Color::White))?;
    writeln!(writer, "  ９ ８ ７ ６ ５ ４ ３ ２ １")?;
    writeln!(writer, "+---------------------------+")?;
    for (rank, rank_label) in Rank::ALL.into_iter().zip(RANKS) {
        let mut row = String::from("|");
        for file in BoardFile::ALL.into_iter().rev() {
            let piece = pos.piece_on(Square::new(file, rank));
            if piece.is_none() {
                row.push_str(" ・");
            } else {
                row.push(if piece.color() == Color::White {
                    'v'
                } else {
                    ' '
                });
                row.push_str(bod_piece_char(piece.piece_type()));
            }
        }
        row.push('|');
        row.push_str(rank_label);
        writeln!(writer, "{row}")?;
    }
    writeln!(writer, "+---------------------------+")?;
    writeln!(writer, "先手の持駒：{}", format_hand(pos, Color::Black))?;
    if pos.side_to_move() == Color::White {
        writeln!(writer, "後手番")?;
    }
    Ok(())
}

/// 局面図の 1 マスに書く 1 文字の駒名。
fn bod_piece_char(piece_type: PieceType) -> &'static str {
    match piece_type {
        PieceType::ProLance => "杏",
        PieceType::ProKnight => "圭",
        PieceType::ProSilver => "全",
        other => piece_label(other, false),
    }
}

/// 持ち駒の欄（`飛　角　歩二　`、無ければ `なし`）。
fn format_hand(pos: &Position, color: Color) -> String {
    const ORDER: [PieceType; 7] = [
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
        PieceType::Pawn,
    ];
    let hand = pos.hand(color);
    let mut text = String::new();
    for piece_type in ORDER {
        let count = hand.count(piece_type);
        if count == 0 {
            continue;
        }
        text.push_str(piece_label(piece_type, false));
        if count > 1 {
            text.push_str(&kanji_count(count));
        }
        text.push('　');
    }
    if text.is_empty() {
        "なし".to_owned()
    } else {
        text
    }
}

/// 持ち駒の枚数（2〜18）の漢数字。
fn kanji_count(n: u32) -> String {
    const DIGITS: [&str; 10] = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    let tens = if n >= 10 { "十" } else { "" };
    format!("{}{}", tens, DIGITS[(n % 10) as usize])
}

fn start_position_for_game(game_id: u32, moves: &[MoveEntry]) -> Result<Position> {
    let first = moves
        .first()
        .ok_or_else(|| anyhow!("game {} has no moves; cannot infer start position", game_id))?;
    let mut pos = Position::new();
    pos.set_sfen(&first.sfen_before).map_err(|e| {
        anyhow!("game {}: failed to parse sfen_before '{}': {}", game_id, first.sfen_before, e)
    })?;
    Ok(pos)
}

fn engine_names_for(meta: Option<&KifMeta>) -> (String, String) {
    let default = ("black".to_string(), "white".to_string());
    let Some(meta) = meta else { return default };
    let black_name = Path::new(&meta.engine_cmd.path_black)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&meta.engine_cmd.path_black)
        .to_string();
    let white_name = Path::new(&meta.engine_cmd.path_white)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&meta.engine_cmd.path_white)
        .to_string();
    let black_opts = &meta.engine_cmd.usi_options_black;
    let white_opts = &meta.engine_cmd.usi_options_white;
    let black_display = if black_opts.is_empty() {
        black_name
    } else {
        format!("{} [{}]", black_name, black_opts.join(", "))
    };
    let white_display = if white_opts.is_empty() {
        white_name
    } else {
        format!("{} [{}]", white_name, white_opts.join(", "))
    };
    (black_display, white_display)
}

/// 指し手1つ分の棋譜風ラベル（消費時間を含まない）。
///
/// `crates/tools/src/replay/` の棋譜プレイヤー TUI から、PSV/JSONL 共通の
/// 手の表示ラベルとして再利用する（PSV には消費時間情報が無いため、
/// 時間表示を含む `format_move_kif` ではなくこちらを直接使う）。
pub(crate) fn format_move_label(ply: u32, pos: &Position, mv: Move) -> String {
    let prefix = if pos.side_to_move() == Color::Black {
        "▲"
    } else {
        "△"
    };
    if mv.is_pass() {
        return format!("{:>4} {}パス", ply, prefix);
    }
    format!("{:>4} {}{}", ply, prefix, move_text(pos, mv))
}

/// 手番の印・手数を付けない指し手の表記（`７六歩(77)`、`５五角打`、`２二角成(88)`）。
fn move_text(pos: &Position, mv: Move) -> String {
    let dest = square_label_kanji(mv.to());
    let (label, from_suffix) = if mv.is_drop() {
        (format!("{}打", piece_label(mv.drop_piece_type(), false)), String::new())
    } else {
        let from = mv.from();
        let piece_type = pos.piece_on(from).piece_type();
        // 成る手は「駒名＋成」（`２二角成`）で書く。成った後の駒名（`２二馬`）で書くと、
        // KIF では既に成っている駒の移動と読まれる。成らない手には何も付けない（「不成」を
        // 読めない KIF の読み手がある）。
        let promotion = if mv.is_promote() { "成" } else { "" };
        let suffix = format!("({}{})", square_file_digit(from), square_rank_digit(from));
        (format!("{}{}", piece_label(piece_type, false), promotion), suffix)
    };
    format!("{dest}{label}{from_suffix}")
}

fn format_move_kif(ply: u32, pos: &Position, mv: Move, elapsed_ms: u64, total_ms: u64) -> String {
    let label = format_move_label(ply, pos, mv);
    let per_move = format_mm_ss(elapsed_ms);
    let total = format_hh_mm_ss(total_ms);
    format!("{}   ({:>5}/{})", label, per_move, total)
}

fn square_label_kanji(sq: Square) -> String {
    format!("{}{}", file_kanji(sq), rank_kanji(sq))
}

fn file_kanji(sq: Square) -> &'static str {
    const FILES: [&str; 10] = ["", "１", "２", "３", "４", "５", "６", "７", "８", "９"];
    let idx = sq.file().to_usi_char().to_digit(10).unwrap_or(1) as usize;
    FILES[idx]
}

fn rank_kanji(sq: Square) -> &'static str {
    const RANKS: [&str; 9] = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];
    let rank = sq.rank().to_usi_char() as u8;
    let idx = (rank - b'a') as usize;
    RANKS.get(idx).copied().unwrap_or("一")
}

fn square_file_digit(sq: Square) -> char {
    sq.file().to_usi_char()
}

fn square_rank_digit(sq: Square) -> char {
    let rank = sq.rank().to_usi_char();
    let idx = (rank as u8 - b'a') + 1;
    char::from_digit(idx as u32, 10).unwrap_or('1')
}

/// `replay::tui` の盤面・持ち駒表示からも再利用する駒名ラベル。
pub(crate) fn piece_label(piece_type: PieceType, promoted: bool) -> &'static str {
    match (piece_type, promoted) {
        (PieceType::Pawn, false) => "歩",
        (PieceType::Pawn, true) => "と",
        (PieceType::Lance, false) => "香",
        (PieceType::Lance, true) => "成香",
        (PieceType::Knight, false) => "桂",
        (PieceType::Knight, true) => "成桂",
        (PieceType::Silver, false) => "銀",
        (PieceType::Silver, true) => "成銀",
        (PieceType::Gold, _) => "金",
        (PieceType::Bishop, false) => "角",
        (PieceType::Bishop, true) => "馬",
        (PieceType::Rook, false) => "飛",
        (PieceType::Rook, true) => "龍",
        (PieceType::King, _) => "玉",
        (PieceType::ProPawn, _) => "と",
        (PieceType::ProLance, _) => "成香",
        (PieceType::ProKnight, _) => "成桂",
        (PieceType::ProSilver, _) => "成銀",
        (PieceType::Horse, _) => "馬",
        (PieceType::Dragon, _) => "龍",
    }
}

/// 1 手に付いていた探索の情報を、ShogiHome と同じ KIF のコメント行（`**評価値=…` など）にする。
///
/// ShogiHome が書く・読む書式に合わせる:
/// - 評価値・詰みは先手から見た向き（ログの値は、指した側 `mover` から見た値）
/// - 読み筋は指した後の局面 `after` から、指した手を除いた KIF の表記（`△３四歩(33)▲２二角成(88)`）。
///   読み筋が指した手から始まらないときは書かない
///
/// 深さ・ノード数のほか、選択深さ・探索時間・NPS も同じ形で添える。
fn eval_comment_lines(
    after: &Position,
    mover: Color,
    played: Move,
    eval: Option<&EvalLog>,
) -> Vec<String> {
    let Some(eval) = eval else { return Vec::new() };
    let color_label = |color: Color| {
        if color == Color::Black {
            "先手"
        } else {
            "後手"
        }
    };
    let mut lines = Vec::new();
    match (eval.score_mate, eval.score_mate_sign, eval.score_cp) {
        (Some(mate), _, _) if mate != 0 => {
            let winner = if mate > 0 { mover } else { !mover };
            lines.push(format!("**詰み={}勝ち:{}手", color_label(winner), mate.unsigned_abs()));
        }
        // 手数の分からない詰み（USI の `score mate +` / `score mate -`）。
        (_, Some(sign), _) if sign != 0 => {
            let winner = if sign > 0 { mover } else { !mover };
            lines.push(format!("**詰み={}勝ち", color_label(winner)));
        }
        (_, _, Some(cp)) => {
            // i32::MIN を反転しても桁あふれしないよう i64 で反転する。
            let cp = if mover == Color::Black {
                i64::from(cp)
            } else {
                -i64::from(cp)
            };
            lines.push(format!("**評価値={cp}"));
        }
        _ => {}
    }
    if let Some(pv) = eval.pv.as_ref()
        && pv.first().and_then(|usi| Move::from_usi_strict(usi)) == Some(played)
    {
        let text = format_pv(after, &pv[1..]);
        if !text.is_empty() {
            lines.push(format!("**読み筋={text}"));
        }
    }
    if let Some(depth) = eval.depth {
        lines.push(format!("**深さ={depth}"));
    }
    if let Some(seldepth) = eval.seldepth {
        lines.push(format!("**選択深さ={seldepth}"));
    }
    if let Some(nodes) = eval.nodes {
        lines.push(format!("**ノード数={nodes}"));
    }
    if let Some(time_ms) = eval.time_ms {
        lines.push(format!("**探索時間={time_ms}ms"));
    }
    if let Some(nps) = eval.nps {
        lines.push(format!("**NPS={nps}"));
    }
    lines
}

/// USI の読み筋を、局面 `pos` から KIF の表記でつなげる（`▲７六歩(77)△３四歩(33)`）。
/// 読めない手・指せない手・パス（KIF の読み筋に書けない）の手前で打ち切る。
fn format_pv(pos: &Position, pv: &[String]) -> String {
    let mut pos = pos.clone();
    let mut text = String::new();
    for usi in pv {
        let Some(mv) = Move::from_usi_strict(usi) else {
            break;
        };
        if mv.is_pass() || !is_legal_game_move(&pos, mv) {
            break;
        }
        text.push(if pos.side_to_move() == Color::Black {
            '▲'
        } else {
            '△'
        });
        text.push_str(&move_text(&pos, mv));
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
    }
    text
}

fn format_mm_ss(ms: u64) -> String {
    let secs = ms / 1000;
    let m = secs / 60;
    let s = secs % 60;
    format!("{:>2}:{:02}", m, s)
}

fn format_hh_mm_ss(ms: u64) -> String {
    let secs = ms / 1000;
    let h = secs / 3600;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_clock_defaults_only_when_missing() {
        for (json, expected_time, expected_inc) in [
            (r#"{"btime":1000,"binc":200}"#, 1000, 200),
            (r#"{"btime":1000,"binc":200,"wtime":0,"winc":0}"#, 0, 0),
            (r#"{"btime":1000,"binc":200,"wtime":0}"#, 0, 200),
            (r#"{"btime":1000,"binc":200,"winc":0}"#, 1000, 0),
            (r#"{"btime":1000,"binc":200,"wtime":6000,"winc":300}"#, 6000, 300),
        ] {
            let settings: KifMetaSettings = serde_json::from_str(json).unwrap();
            assert_eq!(settings.white_time(), expected_time);
            assert_eq!(settings.white_increment(), expected_inc);
            let meta = KifMeta {
                settings,
                ..Default::default()
            };
            let game = GameLog {
                moves: vec![MoveEntry {
                    game_id: 1,
                    ply: 1,
                    sfen_before: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
                        .into(),
                    move_usi: "resign".into(),
                    engine: None,
                    timed_out: false,
                    elapsed_ms: None,
                    eval: None,
                }],
                ..Default::default()
            };
            let mut output = Vec::new();
            export_game_to_kif(&mut output, Some(&meta), 1, &game).unwrap();
            let kif = String::from_utf8(output).unwrap();
            assert!(kif.contains(&format!("持ち時間：先手1000ms / 後手{expected_time}ms")));
            assert!(kif.contains(&format!("加算時間：先手200ms / 後手{expected_inc}ms")));
        }
    }

    #[test]
    fn filter_matches_all_when_empty() {
        let f = GameFilter::default();
        assert!(f.matches_id(1));
        assert!(f.matches_id(999));
    }

    #[test]
    fn filter_matches_specific_ids() {
        let f = GameFilter {
            game_ids: vec![3, 7],
            ..Default::default()
        };
        assert!(!f.matches_id(1));
        assert!(f.matches_id(3));
        assert!(!f.matches_id(5));
        assert!(f.matches_id(7));
    }

    /// 最小サンプル jsonl から KIF が出力できることを end-to-end で検証する。
    /// メタ撤去や move ログ schema 変更等の regression を検知する目的。
    #[test]
    fn convert_minimal_jsonl_emits_kif() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("games.jsonl");
        let output = dir.path().join("out");

        // 1 局のみ・2 手指して 引き分け で終了する最小ログ
        let startpos = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let mut f = std::fs::File::create(&input).expect("create input");
        writeln!(
            f,
            r#"{{"type":"meta","timestamp":"2026-05-05T00:00:00+09:00","settings":{{"btime":1000,"wtime":1000}},"engine_cmd":{{"path_black":"/p/black","path_white":"/p/white"}}}}"#
        ).unwrap();
        writeln!(
            f,
            r#"{{"type":"move","game_id":1,"ply":1,"side_to_move":"b","sfen_before":"{}","move_usi":"7g7f","engine":"black","elapsed_ms":100,"think_limit_ms":1000,"timed_out":false}}"#,
            startpos
        ).unwrap();
        writeln!(
            f,
            r#"{{"type":"move","game_id":1,"ply":2,"side_to_move":"w","sfen_before":"lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2","move_usi":"3c3d","engine":"white","elapsed_ms":200,"think_limit_ms":1000,"timed_out":false}}"#
        ).unwrap();
        writeln!(
            f,
            r#"{{"type":"result","game_id":1,"outcome":"draw","reason":"max_moves","plies":2}}"#
        )
        .unwrap();
        drop(f);

        let written = convert_jsonl_to_kif(&input, &output, &GameFilter::default())
            .expect("convert_jsonl_to_kif");
        assert_eq!(written.len(), 1);
        let kif = std::fs::read_to_string(&written[0]).expect("read kif");
        assert!(kif.contains("先手：black"), "kif:\n{}", kif);
        assert!(kif.contains("後手：white"), "kif:\n{}", kif);
        assert!(kif.contains("▲"), "kif:\n{}", kif);
        assert!(kif.contains("△"), "kif:\n{}", kif);
        assert!(kif.contains("まで2手で持将棋"), "kif:\n{}", kif);
    }

    #[test]
    fn convert_errors_when_no_games_match_filter() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("games.jsonl");
        let output = dir.path().join("out");
        let mut f = std::fs::File::create(&input).expect("create input");
        writeln!(
            f,
            r#"{{"type":"result","game_id":1,"outcome":"draw","reason":"max_moves","plies":0}}"#
        )
        .unwrap();
        drop(f);
        let filter = GameFilter {
            game_ids: vec![999],
            ..Default::default()
        };
        let err = convert_jsonl_to_kif(&input, &output, &filter)
            .expect_err("should bail on empty filter result");
        assert!(format!("{err}").contains("no games matched filter"), "err: {err}");
    }
    fn write_terminal_game(path: &Path, terminal: &str) -> Result<()> {
        let row = serde_json::json!({
            "type": "move", "game_id": 1, "ply": 1,
            "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "move_usi": terminal,
        });
        std::fs::write(path, format!("{row}\n"))?;
        Ok(())
    }

    #[test]
    fn terminal_pseudo_moves_are_exported_without_parsing_as_board_moves() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("games.jsonl");
        let output = dir.path().join("game.kif");
        for terminal in ["resign", "win", "timeout", "illegal", "none"] {
            write_terminal_game(&input, terminal)?;
            convert_jsonl_to_kif(&input, &output, &GameFilter::default())?;
            assert!(std::fs::read_to_string(&output)?.contains("まで0手で中断"));
        }
        Ok(())
    }

    #[test]
    fn conversion_rejects_input_aliases_without_changing_input() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("games.jsonl");
        write_terminal_game(&input, "resign")?;
        let original = std::fs::read(&input)?;
        assert!(convert_jsonl_to_kif(&input, &input, &GameFilter::default()).is_err());
        let alias = dir.path().join("alias.kif");
        std::fs::hard_link(&input, &alias)?;
        assert!(convert_jsonl_to_kif(&input, &alias, &GameFilter::default()).is_err());
        assert_eq!(std::fs::read(&input)?, original);
        assert_eq!(std::fs::read(&alias)?, original);
        Ok(())
    }

    #[test]
    fn conversion_error_preserves_existing_kif() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("games.jsonl");
        let output = dir.path().join("game.kif");
        write_terminal_game(&input, "bad_move")?;
        std::fs::write(&output, b"previous complete game")?;
        assert!(convert_jsonl_to_kif(&input, &output, &GameFilter::default()).is_err());
        assert_eq!(std::fs::read(&output)?, b"previous complete game");
        Ok(())
    }

    #[test]
    fn exchanged_colors_use_move_engines_and_keep_matching_options() -> Result<()> {
        let meta: KifMeta = serde_json::from_value(serde_json::json!({
            "engine_cmd": {
                "path_black": "engine_a", "path_white": "engine_b",
                "label_black": "a", "label_white": "b",
                "usi_options_black": ["Style=A"], "usi_options_white": ["Style=B"],
            }
        }))?;
        let first: MoveEntry = serde_json::from_value(serde_json::json!({
            "game_id": 2, "ply": 1,
            "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "move_usi": "7g7f", "engine": "b",
        }))?;
        let second: MoveEntry = serde_json::from_value(serde_json::json!({
            "game_id": 2, "ply": 2,
            "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
            "move_usi": "resign", "engine": "a",
        }))?;
        let game = GameLog {
            moves: vec![first, second],
            result: None,
        };
        let mut output = Vec::new();
        export_game_to_kif(&mut output, Some(&meta), 2, &game)?;
        let text = String::from_utf8(output)?;
        assert!(text.contains("先手：engine_b [Style=B]"));
        assert!(text.contains("後手：engine_a [Style=A]"));
        Ok(())
    }
    #[test]
    fn a_legal_bestmove_received_after_timeout_is_not_exported_as_a_move() -> Result<()> {
        let game = GameLog {
            moves: vec![serde_json::from_value(serde_json::json!({
                "game_id": 1, "ply": 1,
                "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
                "move_usi": "7g7f", "timed_out": true,
            }))?],
            result: Some(serde_json::from_value(serde_json::json!({
                "outcome": "white_win", "reason": "timeout", "plies": 1,
            }))?),
        };
        let mut output = Vec::new();
        export_game_to_kif(&mut output, None, 1, &game)?;
        let text = String::from_utf8(output)?;
        assert!(!text.contains("▲"));
        assert!(text.contains("   1 切れ負け"));
        assert!(text.contains("まで0手で後手の勝ち"));
        Ok(())
    }
    #[test]
    fn explicit_labels_take_priority_over_shared_binary_name_in_both_colors() -> Result<()> {
        let meta: KifMeta = serde_json::from_value(serde_json::json!({
            "engine_cmd": {
                "path_black": "engine", "path_white": "engine",
                "label_black": "candidate", "label_white": "engine",
                "usi_options_black": ["EvalFile=candidate.bin"],
                "usi_options_white": ["EvalFile=base.bin"],
            }
        }))?;
        for exchanged in [false, true] {
            let (black_label, white_label, black_eval, white_eval) = if exchanged {
                ("engine", "candidate", "base.bin", "candidate.bin")
            } else {
                ("candidate", "engine", "candidate.bin", "base.bin")
            };
            let game = GameLog {
                moves: vec![
                    serde_json::from_value(serde_json::json!({
                        "game_id": 1, "ply": 1,
                        "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
                        "move_usi": "7g7f", "engine": black_label,
                    }))?,
                    serde_json::from_value(serde_json::json!({
                        "game_id": 1, "ply": 2,
                        "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
                        "move_usi": "resign", "engine": white_label,
                    }))?,
                ],
                result: None,
            };
            let mut output = Vec::new();
            export_game_to_kif(&mut output, Some(&meta), 1, &game)?;
            let text = String::from_utf8(output)?;
            assert!(text.contains(&format!("先手：engine [EvalFile={black_eval}]")));
            assert!(text.contains(&format!("後手：engine [EvalFile={white_eval}]")));
        }
        Ok(())
    }

    #[test]
    fn ambiguous_binary_names_do_not_select_either_option_set() -> Result<()> {
        let meta: KifMeta = serde_json::from_value(serde_json::json!({
            "engine_cmd": {
                "path_black": "engine", "path_white": "engine",
                "usi_options_black": ["EvalFile=candidate.bin"],
                "usi_options_white": ["EvalFile=base.bin"],
            }
        }))?;
        let game = GameLog {
            moves: vec![serde_json::from_value(serde_json::json!({
                "game_id": 1, "ply": 1,
                "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
                "move_usi": "resign", "engine": "engine",
            }))?],
            result: None,
        };
        let mut output = Vec::new();
        export_game_to_kif(&mut output, Some(&meta), 1, &game)?;
        let text = String::from_utf8(output)?;
        assert!(text.lines().any(|line| line == "先手：engine"));
        Ok(())
    }

    fn kif_for(moves: serde_json::Value, result: Option<serde_json::Value>) -> Result<String> {
        let game = GameLog {
            moves: serde_json::from_value(moves)?,
            result: result.map(serde_json::from_value).transpose()?,
        };
        let mut output = Vec::new();
        export_game_to_kif(&mut output, None, 1, &game)?;
        Ok(String::from_utf8(output)?)
    }

    #[test]
    fn promotion_is_written_after_the_unpromoted_piece_name() -> Result<()> {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3")
            .map_err(|e| anyhow!("{e}"))?;
        let label = |usi: &str| format_move_label(3, &pos, Move::from_usi(usi).unwrap());
        assert_eq!(label("8h2b+"), "   3 ▲２二角成(88)");
        assert_eq!(label("8h2b"), "   3 ▲２二角(88)");
        assert_eq!(label("8h7g"), "   3 ▲７七角(88)");

        // 後手の成り（7〜9 段）と、成駒の移動。
        pos.set_sfen("4k4/9/9/9/9/9/2s6/9/4K4 w - 1").map_err(|e| anyhow!("{e}"))?;
        let label = |usi: &str| format_move_label(1, &pos, Move::from_usi(usi).unwrap());
        assert_eq!(label("7g7h+"), "   1 △７八銀成(77)");
        assert_eq!(label("7g7h"), "   1 △７八銀(77)");
        pos.set_sfen("4k4/9/9/9/9/9/2+s6/9/4K4 w - 1").map_err(|e| anyhow!("{e}"))?;
        let label = |usi: &str| format_move_label(1, &pos, Move::from_usi(usi).unwrap());
        assert_eq!(label("7g7h"), "   1 △７八成銀(77)");
        Ok(())
    }

    #[test]
    fn terminal_lines_follow_the_side_to_move_and_the_recorded_result() {
        use Color::{Black, White};
        // 先手が指した後（後手番）に後手が投了・時間切れ → 先手の勝ち。
        assert_eq!(terminal_move_label("resign", "black_win", White, false), Some("投了"));
        assert_eq!(terminal_move_label("timeout", "black_win", White, false), Some("切れ負け"));
        assert_eq!(terminal_move_label("time_up", "black_win", White, true), Some("切れ負け"));
        // 勝敗と食い違う語は書かない。
        assert_eq!(terminal_move_label("resign", "white_win", White, false), None);
        // CSA の理由行の無い #WIN は投了扱い、selfplay の win は入玉宣言。
        assert_eq!(terminal_move_label("win", "black_win", White, true), Some("投了"));
        assert_eq!(terminal_move_label("win", "black_win", Black, false), Some("入玉勝ち"));
        assert_eq!(
            terminal_move_label("win_declaration", "white_win", White, true),
            Some("入玉勝ち")
        );
        assert_eq!(
            terminal_move_label("sennichite_perpetual_check", "white_win", White, false),
            Some("反則勝ち")
        );
        // 逃げる手で循環が閉じたとき（王手を掛け続けた先手が次に指す番）。
        assert_eq!(
            terminal_move_label("sennichite_perpetual_check", "white_win", Black, false),
            Some("反則負け")
        );
        assert_eq!(
            terminal_move_label("oute_sennichite", "white_win", Black, true),
            Some("反則負け")
        );
        assert_eq!(terminal_move_label("oute_sennichite", "draw", Black, true), None);
        assert_eq!(terminal_move_label("sennichite", "draw", Black, false), Some("千日手"));
        assert_eq!(terminal_move_label("max_moves", "draw", Black, false), Some("持将棋"));
        assert_eq!(terminal_move_label("max_moves", "draw", Black, true), None);
        assert_eq!(terminal_move_label("adjudication_draw", "draw", Black, false), None);
    }

    #[test]
    fn non_hirate_start_is_written_as_a_board_diagram() -> Result<()> {
        let sfen = "l1sg3nl/1r3kg2/p1npppsp1/2p3p1p/1p5P1/2P2PP1P/PPSPP1N2/5S1R1/LN1GKG2L w Bb 24";
        let text = kif_for(
            serde_json::json!([{"game_id": 1, "ply": 1, "sfen_before": sfen, "move_usi": "7a6b"}]),
            None,
        )?;
        assert!(!text.contains("手合割"), "{text}");
        assert!(!text.contains("開始局面"), "{text}");
        assert!(text.contains("後手の持駒：角　\n"), "{text}");
        assert!(text.contains("|v香 ・v銀v金 ・ ・ ・v桂v香|一\n"), "{text}");
        assert!(text.contains("| 香 桂 ・ 金 玉 金 ・ ・ 香|九\n"), "{text}");
        assert!(text.contains("先手の持駒：角　\n後手番\n"), "{text}");
        assert!(text.contains("   1 △６二銀(71)"), "{text}");
        Ok(())
    }

    #[test]
    fn resignation_is_a_terminal_line_and_not_counted_as_a_move() -> Result<()> {
        let text = kif_for(
            serde_json::json!([{
                "game_id": 1, "ply": 1, "move_usi": "7g7f",
                "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            }]),
            Some(serde_json::json!({"outcome": "black_win", "reason": "resign", "plies": 2})),
        )?;
        assert!(text.contains("手合割：平手\n"), "{text}");
        assert!(text.contains("\n   2 投了\n"), "{text}");
        assert!(text.contains("まで1手で先手の勝ち"), "{text}");
        Ok(())
    }

    #[test]
    fn draw_summaries_name_repetition_and_max_moves() -> Result<()> {
        let moves = serde_json::json!([{
            "game_id": 1, "ply": 1, "move_usi": "7g7f",
            "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        }]);
        for (reason, line, summary) in [
            ("sennichite", "   2 千日手\n", "まで1手で千日手"),
            ("max_moves", "   2 持将棋\n", "まで1手で持将棋"),
            ("adjudication_draw", "", "まで1手で引き分け"),
        ] {
            let text = kif_for(
                moves.clone(),
                Some(serde_json::json!({"outcome": "draw", "reason": reason, "plies": 1})),
            )?;
            assert!(text.contains(line), "{text}");
            assert!(text.contains(summary), "{text}");
        }
        Ok(())
    }

    #[test]
    fn hand_counts_are_kanji_numerals() {
        assert_eq!(kanji_count(2), "二");
        assert_eq!(kanji_count(10), "十");
        assert_eq!(kanji_count(18), "十八");
    }

    #[test]
    fn kif_datetime_keeps_the_recorded_offset() {
        assert_eq!(
            format_kif_datetime("2026-11-06T18:53:43.665256126+00:00"),
            "2026/11/06 18:53:43"
        );
        assert_eq!(format_kif_datetime("-"), "-");
    }

    #[test]
    fn eval_comments_follow_the_shogihome_format() -> Result<()> {
        let start = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let text = kif_for(
            serde_json::json!([
                {
                    "game_id": 1, "ply": 1, "move_usi": "7g7f", "sfen_before": start,
                    // 角を取ったあと、取った角を打つ手まで読む。
                    "eval": {"score_cp": 50, "depth": 20, "nodes": 1000,
                             "pv": ["7g7f", "3c3d", "8h2b+", "3a2b", "B*4e"]},
                },
                {
                    "game_id": 1, "ply": 2, "move_usi": "3c3d",
                    "sfen_before": "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
                    // 末尾の壊れた手（8c8dx）の手前で打ち切る。
                    "eval": {"score_cp": 30, "pv": ["3c3d", "2g2f", "8c8dx", "5c5d"]},
                },
                {
                    "game_id": 1, "ply": 3, "move_usi": "2g2f",
                    "sfen_before": "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3",
                    // 指した手から始まらない読み筋は書かない。
                    "eval": {"score_mate": -5, "pv": ["8h2b+", "3a2b"]},
                },
                {
                    "game_id": 1, "ply": 4, "move_usi": "8c8d",
                    "sfen_before": "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P4P1/PP1PPPP1P/1B5R1/LNSGKGSNL w - 4",
                    "eval": {"score_mate_sign": 1, "pv": ["8c8dx", "2f2e"]},
                },
                {
                    "game_id": 1, "ply": 5, "move_usi": "2f2e",
                    "sfen_before": "lnsgkgsnl/1r5b1/p1pppp1pp/1p4p2/9/2P4P1/PP1PPPP1P/1B5R1/LNSGKGSNL b - 5",
                    // 空きマスから動く手（5d5e）の手前で打ち切り、指した後の手順が無ければ書かない。
                    "eval": {"score_mate": 3, "pv": ["2f2e", "5d5e"]},
                },
                {
                    "game_id": 1, "ply": 6, "move_usi": "8d8e",
                    "sfen_before": "lnsgkgsnl/1r5b1/p1pppp1pp/1p4p2/7P1/2P6/PP1PPPP1P/1B5R1/LNSGKGSNL w - 6",
                    "eval": {"score_cp": -2147483648},
                },
            ]),
            None,
        )?;
        assert!(!text.contains("*info"), "{text}");
        assert!(
            text.contains("**評価値=50\n**読み筋=△３四歩(33)▲２二角成(88)△２二銀(31)▲４五角打\n**深さ=20\n**ノード数=1000\n"),
            "{text}"
        );
        // 後手の手番の値は、符号を反転して先手から見た値にする。
        assert!(text.contains("**評価値=-30\n**読み筋=▲２六歩(27)\n"), "{text}");
        assert!(text.contains("**詰み=後手勝ち:5手\n   4"), "{text}");
        assert!(text.contains("**詰み=後手勝ち\n   5"), "{text}");
        assert!(text.contains("**詰み=先手勝ち:3手\n   6"), "{text}");
        assert!(text.contains("**評価値=2147483648\n"), "{text}");
        Ok(())
    }
}
