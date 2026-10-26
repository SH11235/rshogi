//! PSV 教師データの手数・入玉・宣言・終局の分布を集計する。

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use rayon::prelude::*;
use rshogi_core::position::Position;
use rshogi_core::types::{Color, EnteringKingRule, Move};
use serde::Serialize;
use tools::output_path::ensure_safe_output_paths;
use tools::packed_sfen::{PackedSfenValue, psv_move16_to_move, unpack_sfen_to_parts};
use tools::position_rules::validate_position;

const RECORD_SIZE: usize = PackedSfenValue::SIZE;
const BUFFER_SIZE: usize = 32 << 20;
const CHUNK_RECORDS: usize = 1 << 18;
/// YaneuraOu の `MOVE_WIN` を move16 に詰めた値。宣言勝ちの局面に bestmove として書かれることがある。
const YO_MOVE16_WIN: u16 = (3 << 7) | 3;

#[derive(Parser)]
#[command(about = "PSV 教師データの手数・入玉・宣言・終局の分布を集計する")]
struct Cli {
    /// 入力 PSV (複数可。ファイルごとに独立して読む)
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// 宣言の判定に使う入玉ルール (USI の EnteringKingRule の値)
    #[arg(long, default_value = "CSARule27")]
    entering_king_rule: String,
    /// 手数の集計幅
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u16).range(1..))]
    ply_bin: u16,
    /// 入力をシャッフル前の棋譜順として読み、対局ごとの終局も集計する
    #[arg(long)]
    games: bool,
    /// 集計結果の JSON 出力先
    #[arg(long)]
    json: Option<PathBuf>,
    /// 並列数 (0 = 全コア)
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

#[derive(Clone, Copy)]
struct Config {
    rule: EnteringKingRule,
    ply_bin: u16,
    games: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zone {
    EnteredBlack,
    EnteredWhite,
    EnteredBoth,
    Advancing,
    Normal,
}

/// 対局の最後の局面から推定した終局。`result` はその局面の手番から見た勝敗。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ending {
    /// bestmove が YaneuraOu の MOVE_WIN
    DeclarationMarked,
    /// 手番側が宣言できる局面で、手番側の勝ち
    DeclarationNow,
    /// 記録された手を指すと相手が宣言できる局面になり、手番側の負け
    DeclarationNext,
    Draw,
    OtherDecisive,
}

struct Classified {
    black_to_move: bool,
    zone: Zone,
    declarable: bool,
    ending: Option<Ending>,
}

#[derive(Default, Clone, Serialize, PartialEq, Eq, Debug)]
struct PositionBucket {
    records: u64,
    entered_black: u64,
    entered_white: u64,
    entered_both: u64,
    advancing: u64,
    declarable: u64,
    result_win: u64,
    result_draw: u64,
    result_loss: u64,
}

#[derive(Default, Clone, Serialize, PartialEq, Eq, Debug)]
struct GameBucket {
    games: u64,
    /// 対局中に一度でも入玉した局面 (先後どちらか) を含む対局
    entered_games: u64,
    black_win: u64,
    white_win: u64,
    draw: u64,
    declaration_marked: u64,
    declaration_now: u64,
    declaration_next: u64,
    other_decisive: u64,
}

#[derive(Default, Serialize, PartialEq, Eq, Debug)]
struct Profile {
    records: u64,
    /// 直前の record と手数が 1 違いで勝敗が反転している record (棋譜順のまま並んでいる割合の目安)
    continued_records: u64,
    totals: PositionBucket,
    by_ply: Vec<PositionBucket>,
    games: Option<GameProfile>,
}

#[derive(Default, Serialize, PartialEq, Eq, Debug)]
struct GameProfile {
    totals: GameBucket,
    /// 最後の局面の手数で分けた集計
    by_end_ply: Vec<GameBucket>,
    /// 最初の局面の手数で分けた対局数
    by_start_ply: Vec<u64>,
}

#[derive(Serialize)]
struct Report<'a> {
    /// UTF-8 でないパスは置換文字で表す
    inputs: Vec<String>,
    entering_king_rule: &'static str,
    ply_bin: u16,
    profile: &'a Profile,
}

impl<'a> Report<'a> {
    fn new(inputs: &[PathBuf], rule: EnteringKingRule, ply_bin: u16, profile: &'a Profile) -> Self {
        Self {
            inputs: inputs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            entering_king_rule: rule.to_usi(),
            ply_bin,
            profile,
        }
    }
}

#[derive(Default)]
struct GameState {
    start_ply: u16,
    entered: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let rule = EnteringKingRule::from_usi(&cli.entering_king_rule)
        .ok_or_else(|| anyhow::anyhow!("Unknown entering king rule: {}", cli.entering_king_rule))?;
    if cli.threads > 0 {
        rayon::ThreadPoolBuilder::new().num_threads(cli.threads).build_global()?;
    }
    if let Some(json) = &cli.json {
        let inputs: Vec<&Path> = cli.inputs.iter().map(PathBuf::as_path).collect();
        ensure_safe_output_paths(&[json.as_path()], &inputs)?;
    }
    let config = Config {
        rule,
        ply_bin: cli.ply_bin,
        games: cli.games,
    };
    let mut profile = Profile {
        games: cli.games.then(GameProfile::default),
        ..Profile::default()
    };
    for input in &cli.inputs {
        profile_file(input, config, CHUNK_RECORDS, &mut profile)
            .with_context(|| format!("Failed to profile {}", input.display()))?;
    }
    print_profile(&profile, config);
    if let Some(json) = &cli.json {
        let report = Report::new(&cli.inputs, rule, cli.ply_bin, &profile);
        std::fs::write(json, serde_json::to_vec_pretty(&report)?)
            .with_context(|| format!("Failed to write {}", json.display()))?;
    }
    Ok(())
}

fn field_ply(record: &[u8]) -> u16 {
    u16::from_le_bytes([record[36], record[37]])
}

fn field_result(record: &[u8]) -> i8 {
    record[38] as i8
}

/// `next` が `prev` と同じ対局の次の局面として並んでいるか。
fn continues(prev: &[u8], next: &[u8]) -> bool {
    field_ply(prev).checked_add(1) == Some(field_ply(next))
        && i16::from(field_result(next)) == -i16::from(field_result(prev))
}

fn checked_records(path: &Path) -> Result<u64> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("Failed to stat {}", path.display()))?
        .len();
    anyhow::ensure!(
        size % RECORD_SIZE as u64 == 0,
        "Input size is not a multiple of {RECORD_SIZE}: {} ({size} bytes)",
        path.display()
    );
    Ok(size / RECORD_SIZE as u64)
}

/// 1 ファイルを chunk ごとに読み、局面の分類を並列に行ってから元の順序で集計する。
///
/// 対局の最後の局面かどうかは次の record を見ないと決まらないため、chunk の最後の
/// record は次の chunk の先頭に回す。
fn profile_file(
    path: &Path,
    config: Config,
    chunk_records: usize,
    profile: &mut Profile,
) -> Result<()> {
    let records = checked_records(path)?;
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(path)?);
    let mut buffer: Vec<u8> = Vec::with_capacity((chunk_records + 1) * RECORD_SIZE);
    let mut prev_tail: Option<[u8; RECORD_SIZE]> = None;
    let mut game = GameState::default();
    let mut read = 0u64;
    let mut processed = 0u64;
    while read < records {
        let take = (records - read).min(chunk_records as u64) as usize;
        let carried = buffer.len();
        buffer.resize(carried + take * RECORD_SIZE, 0);
        reader.read_exact(&mut buffer[carried..])?;
        read += take as u64;
        let at_eof = read == records;
        let usable = if at_eof {
            buffer.len()
        } else {
            buffer.len() - RECORD_SIZE
        };
        process_chunk(
            &buffer[..usable],
            &buffer[usable..],
            processed,
            config,
            &mut prev_tail,
            &mut game,
            profile,
        )?;
        processed += (usable / RECORD_SIZE) as u64;
        buffer.drain(..usable);
    }
    Ok(())
}

/// `chunk` の record を集計する。`next` は `chunk` の直後の record (ファイル末尾なら空)、
/// `first_row` は `chunk` の先頭のファイル内の record 番号。
fn process_chunk(
    chunk: &[u8],
    next: &[u8],
    first_row: u64,
    config: Config,
    prev_tail: &mut Option<[u8; RECORD_SIZE]>,
    game: &mut GameState,
    profile: &mut Profile,
) -> Result<()> {
    let (records, rest) = chunk.as_chunks::<RECORD_SIZE>();
    anyhow::ensure!(rest.is_empty(), "chunk is not a multiple of {RECORD_SIZE} bytes");
    let is_end: Vec<bool> = (0..records.len())
        .map(|i| {
            let following = records.get(i + 1).map_or(next, |r| r.as_slice());
            following.is_empty() || !continues(&records[i], following)
        })
        .collect();
    let classified: Vec<Result<Classified>> = records
        .par_iter()
        .zip(is_end.par_iter())
        .map_init(Position::new, |pos, (record, &end)| {
            classify(pos, record, config, config.games && end)
        })
        .collect();
    for (offset, ((record, end), classified)) in
        records.iter().zip(is_end).zip(classified).enumerate()
    {
        let classified =
            classified.with_context(|| format!("at record {}", first_row + offset as u64))?;
        let ply = field_ply(record);
        let result = field_result(record);
        let starts_game = prev_tail.as_ref().is_none_or(|prev| !continues(prev, record));
        if !starts_game {
            profile.continued_records += 1;
        }
        profile.records += 1;
        let bin = usize::from(ply / config.ply_bin);
        if profile.by_ply.len() <= bin {
            profile.by_ply.resize(bin + 1, PositionBucket::default());
        }
        add_position(&mut profile.totals, &classified, result);
        add_position(&mut profile.by_ply[bin], &classified, result);
        if let Some(games) = profile.games.as_mut() {
            if starts_game {
                *game = GameState {
                    start_ply: ply,
                    entered: false,
                };
            }
            game.entered |= matches!(
                classified.zone,
                Zone::EnteredBlack | Zone::EnteredWhite | Zone::EnteredBoth
            );
            if end {
                let ending = classified
                    .ending
                    .ok_or_else(|| anyhow::anyhow!("ending is not classified for a game end"))?;
                add_game(
                    games,
                    game,
                    ply,
                    result,
                    classified.black_to_move,
                    ending,
                    config.ply_bin,
                );
            }
        }
        *prev_tail = Some(*record);
    }
    Ok(())
}

fn classify(pos: &mut Position, record: &[u8], config: Config, end: bool) -> Result<Classified> {
    let psv =
        PackedSfenValue::from_bytes(record).ok_or_else(|| anyhow::anyhow!("PSV parse failed"))?;
    anyhow::ensure!(
        (-1..=1).contains(&psv.game_result),
        "game_result is out of range: {}",
        psv.game_result
    );
    let parts =
        unpack_sfen_to_parts(&psv.sfen).map_err(|e| anyhow::anyhow!("unpack failed: {e}"))?;
    pos.set_from_parts(&parts.board, &parts.hands, parts.side_to_move)
        .context("invalid position")?;
    if let Some((category, reason)) = validate_position(pos) {
        anyhow::bail!("invalid position ({category}): {reason}");
    }
    let zone = zone_of(pos);
    let declarable = pos.declaration_win(config.rule) != Move::NONE;
    let ending = end.then(|| ending_of(pos, &psv, declarable, config.rule));
    Ok(Classified {
        black_to_move: parts.side_to_move == Color::Black,
        zone,
        declarable,
        ending,
    })
}

/// 先手玉が 1〜3 段目・後手玉が 7〜9 段目にあれば入玉、どちらかの玉が 4〜6 段目なら advancing。
/// advancing の境界は `tools::king_zone::classify` と同じ。
fn zone_of(pos: &Position) -> Zone {
    let black = pos.king_square(Color::Black).rank() as usize;
    let white = pos.king_square(Color::White).rank() as usize;
    match (black <= 2, white >= 6) {
        (true, true) => Zone::EnteredBoth,
        (true, false) => Zone::EnteredBlack,
        (false, true) => Zone::EnteredWhite,
        (false, false) if (3..=5).contains(&black) || (3..=5).contains(&white) => Zone::Advancing,
        (false, false) => Zone::Normal,
    }
}

fn ending_of(
    pos: &mut Position,
    psv: &PackedSfenValue,
    declarable: bool,
    rule: EnteringKingRule,
) -> Ending {
    match psv.game_result {
        0 => return Ending::Draw,
        1 if psv.move16 == YO_MOVE16_WIN => return Ending::DeclarationMarked,
        1 if declarable => return Ending::DeclarationNow,
        _ => {}
    }
    if psv.game_result < 0 && opponent_declares_after(pos, psv.move16, rule) {
        Ending::DeclarationNext
    } else {
        Ending::OtherDecisive
    }
}

/// 記録された手を指した局面で、相手 (指した後の手番側) が宣言できるか。手が非合法なら false。
fn opponent_declares_after(pos: &mut Position, move16: u16, rule: EnteringKingRule) -> bool {
    let Some(mv) = pos.to_move(psv_move16_to_move(move16)) else {
        return false;
    };
    if mv == Move::NONE || !pos.pseudo_legal(mv) || !pos.is_legal(mv) {
        return false;
    }
    let gives_check = pos.gives_check(mv);
    pos.do_move(mv, gives_check);
    pos.declaration_win(rule) != Move::NONE
}

fn add_position(bucket: &mut PositionBucket, classified: &Classified, result: i8) {
    bucket.records += 1;
    match classified.zone {
        Zone::EnteredBlack => bucket.entered_black += 1,
        Zone::EnteredWhite => bucket.entered_white += 1,
        Zone::EnteredBoth => bucket.entered_both += 1,
        Zone::Advancing => bucket.advancing += 1,
        Zone::Normal => {}
    }
    bucket.declarable += u64::from(classified.declarable);
    match result.signum() {
        1 => bucket.result_win += 1,
        0 => bucket.result_draw += 1,
        _ => bucket.result_loss += 1,
    }
}

fn add_game(
    games: &mut GameProfile,
    game: &GameState,
    end_ply: u16,
    result: i8,
    black_to_move: bool,
    ending: Ending,
    ply_bin: u16,
) {
    let end_bin = usize::from(end_ply / ply_bin);
    if games.by_end_ply.len() <= end_bin {
        games.by_end_ply.resize(end_bin + 1, GameBucket::default());
    }
    let start_bin = usize::from(game.start_ply / ply_bin);
    if games.by_start_ply.len() <= start_bin {
        games.by_start_ply.resize(start_bin + 1, 0);
    }
    games.by_start_ply[start_bin] += 1;
    for bucket in [&mut games.totals, &mut games.by_end_ply[end_bin]] {
        bucket.games += 1;
        bucket.entered_games += u64::from(game.entered);
        match (result.signum(), black_to_move) {
            (0, _) => bucket.draw += 1,
            (1, true) | (-1, false) => bucket.black_win += 1,
            _ => bucket.white_win += 1,
        }
        match ending {
            Ending::DeclarationMarked => bucket.declaration_marked += 1,
            Ending::DeclarationNow => bucket.declaration_now += 1,
            Ending::DeclarationNext => bucket.declaration_next += 1,
            Ending::Draw | Ending::OtherDecisive => {}
        }
        bucket.other_decisive += u64::from(ending == Ending::OtherDecisive);
    }
}

fn pct(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 * 100.0 / total as f64
    }
}

fn print_profile(profile: &Profile, config: Config) {
    println!(
        "records={} continued={:.2}% rule={}",
        profile.records,
        pct(profile.continued_records, profile.records),
        config.rule.to_usi()
    );
    println!(
        "ply\trecords\tentered%\tblack%\twhite%\tboth%\tadvancing%\tdeclarable\twin%\tdraw%\tloss%"
    );
    let print_row = |label: String, b: &PositionBucket| {
        let entered = b.entered_black + b.entered_white + b.entered_both;
        println!(
            "{label}\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{}\t{:.1}\t{:.1}\t{:.1}",
            b.records,
            pct(entered, b.records),
            pct(b.entered_black, b.records),
            pct(b.entered_white, b.records),
            pct(b.entered_both, b.records),
            pct(b.advancing, b.records),
            b.declarable,
            pct(b.result_win, b.records),
            pct(b.result_draw, b.records),
            pct(b.result_loss, b.records),
        );
    };
    for (i, b) in profile.by_ply.iter().enumerate().filter(|(_, b)| b.records > 0) {
        print_row(bin_label(i, config.ply_bin), b);
    }
    print_row("all".to_string(), &profile.totals);
    let Some(games) = &profile.games else {
        return;
    };
    println!();
    println!(
        "end_ply\tgames\tentered_games%\tblack_win%\twhite_win%\tdraw%\tdecl_marked\tdecl_now\tdecl_next\tother_decisive"
    );
    let print_game = |label: String, b: &GameBucket| {
        println!(
            "{label}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{}\t{}\t{}",
            b.games,
            pct(b.entered_games, b.games),
            pct(b.black_win, b.games),
            pct(b.white_win, b.games),
            pct(b.draw, b.games),
            b.declaration_marked,
            b.declaration_now,
            b.declaration_next,
            b.other_decisive,
        );
    };
    for (i, b) in games.by_end_ply.iter().enumerate().filter(|(_, b)| b.games > 0) {
        print_game(bin_label(i, config.ply_bin), b);
    }
    print_game("all".to_string(), &games.totals);
    println!();
    println!("start_ply\tgames");
    for (i, &count) in games.by_start_ply.iter().enumerate().filter(|(_, c)| **c > 0) {
        println!("{}\t{count}", bin_label(i, config.ply_bin));
    }
}

fn bin_label(bin: usize, ply_bin: u16) -> String {
    let lo = bin * usize::from(ply_bin);
    format!("{lo}-{}", lo + usize::from(ply_bin) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tools::packed_sfen::{move_to_psv_move16, pack_position};

    /// 先手が 27 点法で宣言できる (先手玉 5 一・敵陣に 10 枚・持ち駒の大駒 4 枚で 30 点)。
    const BLACK_DECLARES: &str = "4K4/4G4/PPPPPPPPP/9/9/9/9/9/4k4 b 2R2B 1";
    /// 後手番で、後手玉 1 五 が 1 六 へ逃げると先手が宣言できる局面になる。
    const BEFORE_BLACK_DECLARES: &str = "4K4/4G4/PPPPPPPPP/9/8k/9/9/9/9 w 2R2B 1";
    const NORMAL: &str = "4k4/9/9/9/9/9/9/9/4K4 b - 1";
    const ADVANCING: &str = "4k4/9/9/9/4K4/9/9/9/9 b - 1";

    fn config(games: bool) -> Config {
        Config {
            rule: EnteringKingRule::Point27,
            ply_bin: 10,
            games,
        }
    }

    fn record(sfen: &str, ply: u16, result: i8, move16: u16) -> [u8; RECORD_SIZE] {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        PackedSfenValue {
            sfen: pack_position(&pos),
            score: 0,
            move16,
            game_ply: ply,
            game_result: result,
            padding: 0,
        }
        .to_bytes()
    }

    fn psv_move(sfen: &str, usi: &str) -> u16 {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        move_to_psv_move16(pos.to_move(Move::from_usi(usi).unwrap()).unwrap())
    }

    /// 3 局: 先手の宣言で終わる局、相手の手の後に先手が宣言できる局、引き分けの局。
    fn three_games() -> Vec<u8> {
        let flee = psv_move(BEFORE_BLACK_DECLARES, "1e1f");
        [
            record(NORMAL, 30, 1, 0),
            record(ADVANCING, 31, -1, 0),
            record(BLACK_DECLARES, 32, 1, 0),
            record(NORMAL, 50, 1, 0),
            record(BEFORE_BLACK_DECLARES, 51, -1, flee),
            record(NORMAL, 7, 0, 0),
            record(NORMAL, 8, 0, 0),
        ]
        .concat()
    }

    fn try_run_files(files: &[&[u8]], config: Config, chunk_records: usize) -> Result<Profile> {
        let dir = tempdir()?;
        let mut profile = Profile {
            games: config.games.then(GameProfile::default),
            ..Profile::default()
        };
        for (i, bytes) in files.iter().enumerate() {
            let path = dir.path().join(format!("in{i}.psv"));
            std::fs::write(&path, bytes)?;
            profile_file(&path, config, chunk_records, &mut profile)?;
        }
        Ok(profile)
    }

    fn run(bytes: &[u8], config: Config, chunk_records: usize) -> Profile {
        try_run_files(&[bytes], config, chunk_records).unwrap()
    }

    #[test]
    fn classifies_zones_and_declarations() {
        let bytes = [
            record(BLACK_DECLARES, 1, 1, 0),
            record(NORMAL, 15, 0, 0),
            record(ADVANCING, 15, -1, 0),
        ]
        .concat();
        let profile = run(&bytes, config(false), 1024);
        assert_eq!(profile.records, 3);
        assert_eq!(profile.totals.entered_both, 1);
        assert_eq!(profile.totals.advancing, 1);
        assert_eq!(profile.totals.declarable, 1);
        assert_eq!(
            (
                profile.totals.result_win,
                profile.totals.result_draw,
                profile.totals.result_loss
            ),
            (1, 1, 1)
        );
        assert_eq!(profile.by_ply[0].records, 1);
        assert_eq!(profile.by_ply[1].records, 2);
        assert!(profile.games.is_none());
    }

    #[test]
    fn segments_games_and_classifies_endings() {
        let profile = run(&three_games(), config(true), 1024);
        let games = profile.games.as_ref().unwrap();
        assert_eq!(games.totals.games, 3);
        assert_eq!(games.totals.declaration_now, 1);
        assert_eq!(games.totals.declaration_next, 1);
        assert_eq!(games.totals.draw, 1);
        assert_eq!(games.totals.other_decisive, 0);
        // 宣言で終わった 2 局はどちらも先手の勝ち
        assert_eq!((games.totals.black_win, games.totals.white_win), (2, 0));
        // 2 局目の局面も先手玉が 5 一にいる
        assert_eq!(games.totals.entered_games, 2);
        assert_eq!(games.by_start_ply, vec![1, 0, 0, 1, 0, 1]);
        assert_eq!(profile.continued_records, 4);
    }

    #[test]
    fn chunk_boundaries_do_not_change_profile() {
        let bytes = three_games();
        let whole = run(&bytes, config(true), 1024);
        for chunk in 1..=7 {
            assert_eq!(run(&bytes, config(true), chunk), whole, "chunk={chunk}");
        }
    }

    #[test]
    fn marked_win_move_is_declaration() {
        let bytes = [record(NORMAL, 3, 1, YO_MOVE16_WIN)].concat();
        let profile = run(&bytes, config(true), 1024);
        assert_eq!(profile.games.unwrap().totals.declaration_marked, 1);
    }

    #[test]
    fn rejects_truncated_input() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("in.psv");
        std::fs::write(&path, [0u8; RECORD_SIZE + 1]).unwrap();
        let mut profile = Profile::default();
        let error = profile_file(&path, config(false), 16, &mut profile).unwrap_err();
        assert!(error.to_string().contains("not a multiple"));
    }

    #[test]
    fn games_do_not_continue_across_files() {
        let first = record(NORMAL, 10, 1, 0);
        let second = record(NORMAL, 11, -1, 0);
        let profile = try_run_files(&[&first, &second], config(true), 1024).unwrap();
        assert_eq!(profile.continued_records, 0);
        assert_eq!(profile.games.unwrap().totals.games, 2);
        // 1 つのファイルに並べると同じ対局になる
        let joined = run(&[first, second].concat(), config(true), 1024);
        assert_eq!(joined.games.unwrap().totals.games, 1);
    }

    #[test]
    fn plain_loss_is_other_decisive_white_win() {
        let profile = run(&record(NORMAL, 40, -1, 0), config(true), 1024);
        let totals = profile.games.unwrap().totals;
        assert_eq!((totals.other_decisive, totals.white_win, totals.black_win), (1, 1, 0));
    }

    #[test]
    fn illegal_or_special_bestmove_is_not_declaration_next() {
        // 玉が 2 マス動く手は非合法
        let illegal = Move::from_usi("1e3e").map(move_to_psv_move16).unwrap();
        let yo_move16_resign = (2 << 7) | 2;
        for move16 in [illegal, yo_move16_resign, 0] {
            let profile = run(&record(BEFORE_BLACK_DECLARES, 51, -1, move16), config(true), 1024);
            let totals = profile.games.unwrap().totals;
            assert_eq!((totals.declaration_next, totals.other_decisive), (0, 1), "move16={move16}");
        }
    }

    #[test]
    fn rejects_illegal_position_with_record_number() {
        let double_pawn = "4k4/9/9/9/9/9/P8/P8/4K4 b - 1";
        let bytes = [record(NORMAL, 1, 0, 0), record(double_pawn, 2, 0, 0)].concat();
        let error = try_run_files(&[&bytes], config(false), 1024).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("at record 1"), "{message}");
        assert!(message.contains("double_pawn"), "{message}");
    }

    #[test]
    fn rejects_out_of_range_game_result() {
        let error = try_run_files(&[&record(NORMAL, 1, 2, 0)], config(false), 1024).unwrap_err();
        assert!(format!("{error:#}").contains("game_result is out of range"));
    }

    #[cfg(unix)]
    #[test]
    fn report_serializes_non_utf8_input_path() {
        use std::os::unix::ffi::OsStrExt;
        let profile = run(&record(NORMAL, 1, 0, 0), config(false), 1024);
        let input = PathBuf::from(std::ffi::OsStr::from_bytes(b"bad\xff.bin"));
        let report = Report::new(&[input], EnteringKingRule::Point27, 10, &profile);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["inputs"][0], "bad\u{fffd}.bin");
        assert_eq!(value["profile"]["records"], 1);
        assert_eq!(value["entering_king_rule"], "CSARule27");
    }
}
