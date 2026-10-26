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
use rshogi_core::types::{Color, Move, PieceType, Square};
use serde::Deserialize;
use serde_json::Value;

use crate::output_path::ensure_safe_output_path;
use crate::selfplay::EvalLog;

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
    plies: u32,
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
    let (mut pos, start_sfen) = start_position_for_game(game_id, &game.moves)?;

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
    writeln!(writer, "開始日時：{}", timestamp)?;
    writeln!(writer, "手合割：平手")?;
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
    writeln!(writer, "開始局面：{}", start_sfen)?;
    writeln!(writer, "手数----指手---------消費時間--")?;

    let mut moves = game.moves.clone();
    moves.sort_by_key(|m| m.ply);
    let mut total_black = 0u64;
    let mut total_white = 0u64;

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
        let line = format_move_kif(entry.ply, &pos, mv, elapsed_ms, total_time);
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
        write_eval_comments(writer, entry.eval.as_ref())?;
    }

    let final_plies = game
        .result
        .as_ref()
        .map(|r| r.plies)
        .or_else(|| game.moves.last().map(|m| m.ply))
        .unwrap_or(0);
    if let Some(res) = game.result.as_ref()
        && res.reason != "max_moves"
    {
        writeln!(writer, "**終了理由={}", res.reason)?;
    }
    let csa = meta.is_some_and(|m| is_csa_jsonl(m.source.as_deref(), m.engine_cmd.note.as_deref()));
    let outcome = game
        .result
        .as_ref()
        .filter(|r| !is_incomplete_result(csa, &r.outcome, Some(&r.reason)))
        .map(|r| r.outcome.as_str());
    let summary = match outcome {
        Some("black_win") => format!("まで{}手で先手の勝ち", final_plies),
        Some("white_win") => format!("まで{}手で後手の勝ち", final_plies),
        Some("draw") => format!("まで{}手で引き分け", final_plies),
        _ => format!("まで{}手で中断", final_plies),
    };
    writeln!(writer, "\n{}", summary)?;
    Ok(())
}

fn start_position_for_game(game_id: u32, moves: &[MoveEntry]) -> Result<(Position, String)> {
    let first = moves
        .first()
        .ok_or_else(|| anyhow!("game {} has no moves; cannot infer start position", game_id))?;
    let mut pos = Position::new();
    pos.set_sfen(&first.sfen_before).map_err(|e| {
        anyhow!("game {}: failed to parse sfen_before '{}': {}", game_id, first.sfen_before, e)
    })?;
    let sfen = pos.to_sfen();
    Ok((pos, sfen))
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
    let dest = square_label_kanji(mv.to());
    let (label, from_suffix) = if mv.is_drop() {
        (format!("{}打", piece_label(mv.drop_piece_type(), false)), String::new())
    } else {
        let from = mv.from();
        let piece = pos.piece_on(from);
        let promoted = piece.piece_type().is_promoted() || mv.is_promote();
        let suffix = format!("({}{})", square_file_digit(from), square_rank_digit(from));
        (piece_label(piece.piece_type(), promoted).to_string(), suffix)
    };
    format!("{:>4} {}{}{}{}", ply, prefix, dest, label, from_suffix)
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

fn write_eval_comments<W: Write>(writer: &mut W, eval: Option<&EvalLog>) -> Result<()> {
    let Some(eval) = eval else { return Ok(()) };
    writeln!(writer, "*info")?;
    if let Some(mate) = eval.score_mate {
        writeln!(writer, "**詰み={}", mate)?;
    } else if let Some(cp) = eval.score_cp {
        writeln!(writer, "**評価値={:+}", cp)?;
    }
    if let Some(depth) = eval.depth {
        writeln!(writer, "**深さ={}", depth)?;
    }
    if let Some(seldepth) = eval.seldepth {
        writeln!(writer, "**選択深さ={}", seldepth)?;
    }
    if let Some(nodes) = eval.nodes {
        writeln!(writer, "**ノード数={}", nodes)?;
    }
    if let Some(time_ms) = eval.time_ms {
        writeln!(writer, "**探索時間={}ms", time_ms)?;
    }
    if let Some(nps) = eval.nps {
        writeln!(writer, "**NPS={}", nps)?;
    }
    if let Some(pv) = eval.pv.as_ref()
        && !pv.is_empty()
    {
        writeln!(writer, "**読み筋={}", pv.join(" "))?;
    }
    Ok(())
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
        assert!(kif.contains("まで2手で引き分け"), "kif:\n{}", kif);
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
            assert!(std::fs::read_to_string(&output)?.contains("まで1手で中断"));
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
        assert!(text.contains("まで1手で後手の勝ち"));
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
}
