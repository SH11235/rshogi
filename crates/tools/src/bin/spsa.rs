use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;
use crossbeam_channel::unbounded;
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tools::selfplay::game::{GameConfig, MoveEvent, run_game};
use tools::selfplay::time_control::TimeControl;
use tools::selfplay::{
    EngineConfig, EngineProcess, GameOutcome, ParsedPosition, load_start_positions,
};
use tools::spsa_param_mapping::MappingTable;

const PARAM_NOT_USED_MARKER: &str = "[[NOT USED]]";
const META_FORMAT_VERSION: u32 = 2;

#[derive(Parser, Debug)]
#[command(author, version, about = "SPSA tuner for USI engines")]
struct Cli {
    /// SPSAパラメータファイル（name,type,v,min,max,c_end,r_end）
    #[arg(long)]
    params: PathBuf,

    /// 反復回数
    #[arg(long, default_value_t = 1)]
    iterations: u32,

    /// 1イテレーションあたり対局数（偶数必須）
    #[arg(long, default_value_t = 2)]
    games_per_iteration: u32,

    /// 対局並列数（worker数）
    #[arg(long, default_value_t = 1)]
    concurrency: usize,

    /// 更新移動量スケール
    #[arg(long, default_value_t = 1.0)]
    mobility: f64,

    /// Fishtest A ratio（A = a_ratio * iterations）
    #[arg(long = "a-ratio", default_value_t = 0.1)]
    a_ratio: f64,

    /// SPSA alpha（a_k 減衰指数）
    #[arg(long, default_value_t = 0.602)]
    alpha: f64,

    /// SPSA gamma（c_k 減衰指数）
    #[arg(long, default_value_t = 0.101)]
    gamma: f64,

    /// 再開メタデータファイル（既定: <params>.meta.json）
    #[arg(long)]
    meta_file: Option<PathBuf>,

    /// 既存メタデータから反復番号を再開する
    #[arg(long, default_value_t = false)]
    resume: bool,

    /// resume時にmetaのschedule不一致を許可する
    #[arg(long, default_value_t = false)]
    force_schedule: bool,

    /// 反復統計CSVの出力先（resume時は追記）。既定: <params>.stats.csv
    #[arg(long)]
    stats_csv: Option<PathBuf>,

    /// 反復統計CSVの出力を無効化する
    #[arg(long, default_value_t = false)]
    no_stats_csv: bool,

    /// 反復統計のseed横断集計CSV（平均・分散）。既定: <params>.stats_aggregate.csv
    #[arg(long)]
    stats_aggregate_csv: Option<PathBuf>,

    /// seed横断集計CSVの出力を無効化する
    #[arg(long, default_value_t = false)]
    no_stats_aggregate_csv: bool,

    /// 反復ごとのパラメータ値履歴CSV（wide形式）。既定: <params>.values.csv
    #[arg(long)]
    param_values_csv: Option<PathBuf>,

    /// パラメータ値履歴CSVの出力を無効化する
    #[arg(long, default_value_t = false)]
    no_param_values_csv: bool,

    /// 乱数seed（単一）
    #[arg(long, conflicts_with = "seeds")]
    seed: Option<u64>,

    /// 乱数seed一覧（カンマ区切り）
    #[arg(long, value_delimiter = ',', num_args = 1.., conflicts_with = "seed")]
    seeds: Option<Vec<u64>>,

    /// エンジンバイナリパス（未指定時: target/release/rshogi-usi）
    #[arg(long)]
    engine_path: Option<PathBuf>,

    /// エンジン追加引数
    #[arg(long, num_args = 1..)]
    engine_args: Option<Vec<String>>,

    /// 追加USIオプション（Name=Value形式、複数指定可）
    #[arg(long = "usi-option", num_args = 1..)]
    usi_options: Option<Vec<String>>,

    /// Threads option
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Hash/USI_Hash (MiB)
    #[arg(long, default_value_t = 256)]
    hash_mb: u32,

    /// 秒読み(ms)。--btime 指定時は無視される。
    #[arg(long, default_value_t = 1000)]
    byoyomi: u64,

    /// フィッシャー: 持ち時間(ms)。指定時は byoyomi を無視しフィッシャーモードになる。
    #[arg(long)]
    btime: Option<u64>,

    /// フィッシャー: 加算時間(ms)。--btime と併用する。
    #[arg(long, default_value_t = 0)]
    binc: u64,

    /// ノード数制限。指定時は時間制御の代わりに `go nodes N` を使用する。
    #[arg(long)]
    nodes: Option<u64>,

    /// 1局あたり最大手数
    #[arg(long, default_value_t = 320)]
    max_moves: u32,

    /// タイムアウト判定マージン(ms)
    #[arg(long, default_value_t = 1000)]
    timeout_margin_ms: u64,

    /// 開始局面ファイル
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// --startpos-file の指定を必須化する
    #[arg(long, default_value_t = false)]
    require_startpos_file: bool,

    /// 単一開始局面（position行またはSFEN）
    #[arg(long)]
    sfen: Option<String>,

    /// 開始局面をランダム選択
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    random_startpos: bool,

    /// チューニング対象パラメータ名を正規表現で限定する
    #[arg(long)]
    active_only_regex: Option<String>,

    /// 早期停止: avg_abs_update の閾値（以下で条件成立）
    #[arg(long)]
    early_stop_avg_abs_update_threshold: Option<f64>,

    /// 早期停止: result_variance の閾値（以下で条件成立）
    #[arg(long)]
    early_stop_result_variance_threshold: Option<f64>,

    /// 早期停止: 条件連続成立回数（0で無効）
    #[arg(long, default_value_t = 0)]
    early_stop_patience: u32,

    /// エンジン側パラメータ名マッピング TOML（例: tune/yo_rshogi_mapping.toml）。
    /// 指定時、`.params` の rshogi 名 (`SPSA_*`) を、setoption する直前にエンジン側名前空間
    /// （例: YaneuraOu の `correction_value_1`）に翻訳し、必要なら符号を反転する。
    /// マッピング表に存在しないパラメータはそのままの名前で送る。
    #[arg(long)]
    engine_param_mapping: Option<PathBuf>,

    /// 正本 `.params` の上書き保護用。`--params` のパスが存在しない時に限り、
    /// `<init-from>` の内容をコピーしてから SPSA を開始する。既に存在する場合は
    /// resume と同じく既存ファイルを読み込んで何もしない。正本（例:
    /// `spsa_params/suisho10_converted.params`）を `--params` に直接渡すと
    /// 反復ごとに上書きされるので、`--params runs/spsa/<ts>/tuned.params
    /// --init-from spsa_params/suisho10_converted.params` のパターンで使う。
    #[arg(long)]
    init_from: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct SpsaParam {
    name: String,
    type_name: String,
    is_int: bool,
    value: f64,
    min: f64,
    max: f64,
    /// Fishtest c_end: 最終摂動幅
    c_end: f64,
    /// Fishtest r_end: 最終学習率係数
    r_end: f64,
    comment: String,
    not_used: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct ScheduleConfig {
    alpha: f64,
    gamma: f64,
    a_ratio: f64,
    mobility: f64,
    total_iterations: u32,
}

/// Fishtest 方式の per-param スケジュール定数。イテレーション開始前に一度だけ計算する。
#[derive(Clone, Copy, Debug)]
struct ParamScheduleConstants {
    /// c_0 = c_end × N^γ
    c_0: f64,
    /// a_0 = r_end × c_end² × (A + N)^α
    a_0: f64,
}

impl ParamScheduleConstants {
    fn compute(
        c_end: f64,
        r_end: f64,
        total_iter: u32,
        a_ratio: f64,
        alpha: f64,
        gamma: f64,
    ) -> Self {
        let n = total_iter as f64;
        let big_a = a_ratio * n;
        let c_0 = c_end * n.powf(gamma);
        let a_end = r_end * c_end * c_end;
        let a_0 = a_end * (big_a + n).powf(alpha);
        Self { c_0, a_0 }
    }

    /// イテレーション k (0-indexed) での (c_k, R_k) を返す。
    fn at_iteration(&self, k: u32, big_a: f64, alpha: f64, gamma: f64) -> (f64, f64) {
        let t = k as f64 + 1.0;
        let c_k = self.c_0 / t.powf(gamma);
        let r_k = self.a_0 / (big_a + t).powf(alpha) / (c_k * c_k);
        (c_k, r_k)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ResumeMetaData {
    format_version: u32,
    params_file: String,
    completed_iterations: u32,
    total_games: usize,
    last_raw_result_mean: f64,
    last_avg_abs_update: f64,
    updated_at_utc: String,
    schedule: ScheduleConfig,
}

#[derive(Clone, Copy, Debug)]
struct IterationStats {
    iteration: u32,
    seed: u64,
    games: u32,
    plus_wins: u32,
    minus_wins: u32,
    draws: u32,
    raw_result: f64,
    active_params: usize,
    avg_abs_shift: f64,
    updated_params: usize,
    avg_abs_update: f64,
    max_abs_update: f64,
    total_games: usize,
}

#[derive(Clone, Copy, Debug)]
struct AggregateIterationStats {
    iteration: u32,
    seed_count: usize,
    games_per_seed: u32,
    raw_result_mean: f64,
    raw_result_variance: f64,
    plus_wins_mean: f64,
    plus_wins_variance: f64,
    minus_wins_mean: f64,
    minus_wins_variance: f64,
    draws_mean: f64,
    draws_variance: f64,
    total_games: usize,
}

#[derive(Clone, Copy, Debug)]
struct GameTask {
    game_idx: u32,
    plus_is_black: bool,
    start_pos_index: usize,
    game_id: u32,
}

#[derive(Clone, Copy)]
struct GameTaskResult {
    game_idx: u32,
    plus_is_black: bool,
    plus_score: f64,
    outcome: GameOutcome,
}

#[derive(Clone, Copy, Debug)]
struct SeedGameStats {
    step_sum: f64,
    plus_wins: u32,
    minus_wins: u32,
    draws: u32,
}

struct SeedRunContext<'a> {
    concurrency: usize,
    base_cfg: &'a EngineConfig,
    params: &'a [SpsaParam],
    plus_values: &'a [f64],
    minus_values: &'a [f64],
    start_positions: &'a [ParsedPosition],
    start_pos_indices: &'a [usize],
    game_cfg: &'a GameConfig,
    tc: TimeControl,
    total_games_start: usize,
    iteration: u32,
    seed_idx: usize,
    seed_count: usize,
    base_seed: u64,
    translator: &'a EngineNameTranslator,
}

/// rshogi `.params` の名前 → エンジン側 USI option 名 への翻訳器
#[derive(Debug, Default)]
struct EngineNameTranslator {
    /// rshogi 名 → (エンジン側名, 符号反転)。
    table: HashMap<String, (String, bool)>,
    /// マッピング表がロードされているか
    enabled: bool,
}

impl EngineNameTranslator {
    fn empty() -> Self {
        Self {
            table: HashMap::new(),
            enabled: false,
        }
    }

    fn from_mapping_file(path: &Path) -> Result<Self> {
        let mapping = MappingTable::load(path)?;
        let table = mapping
            .mappings
            .iter()
            .map(|m| (m.rshogi.clone(), (m.yo.clone(), m.sign_flip)))
            .collect();
        Ok(Self {
            table,
            enabled: true,
        })
    }

    /// `value` を必要に応じて符号反転し、エンジン側に送る (name, value) を返す。
    /// マッピング表にない name はそのまま通す。
    fn translate<'a>(&'a self, name: &'a str, value: f64) -> (&'a str, f64) {
        match self.table.get(name) {
            Some((engine_name, sign_flip)) => {
                let v = if *sign_flip { -value } else { value };
                (engine_name.as_str(), v)
            }
            None => (name, value),
        }
    }

    fn len(&self) -> usize {
        self.table.len()
    }

    /// マッピング表がロードされているか
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// rshogi 名がマッピング表に登録されているか
    fn is_mapped(&self, rshogi_name: &str) -> bool {
        self.table.contains_key(rshogi_name)
    }
}

#[derive(Clone, Copy, Debug)]
struct EarlyStopConfig {
    avg_abs_update_threshold: f64,
    result_variance_threshold: f64,
    patience: u32,
}

fn default_meta_path(params_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.meta.json", params_path.display()))
}

fn default_param_values_csv_path(params_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.values.csv", params_path.display()))
}

fn default_stats_csv_path(params_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.stats.csv", params_path.display()))
}

fn default_stats_aggregate_csv_path(params_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.stats_aggregate.csv", params_path.display()))
}

fn schedule_matches(lhs: ScheduleConfig, rhs: ScheduleConfig) -> bool {
    const EPS: f64 = 1e-12;
    (lhs.alpha - rhs.alpha).abs() <= EPS
        && (lhs.gamma - rhs.gamma).abs() <= EPS
        && (lhs.a_ratio - rhs.a_ratio).abs() <= EPS
        && (lhs.mobility - rhs.mobility).abs() <= EPS
        && lhs.total_iterations == rhs.total_iterations
}

fn is_param_active(
    param: &SpsaParam,
    active_only_regex: Option<&Regex>,
    translator: &EngineNameTranslator,
) -> bool {
    if param.not_used {
        return false;
    }
    if let Some(re) = active_only_regex
        && !re.is_match(&param.name)
    {
        return false;
    }
    // P1: マッピング表がロード済みかつ name が未マッピングの場合、エンジン側で
    // setoption が黙ってスキップされるため SPSA で摂動・更新するのは無駄かつ有害
    // （unmapped.rshogi 系の値がランダムウォークして .params を汚染する）。
    // ここで active 集合から除外する。
    if translator.is_enabled() && !translator.is_mapped(&param.name) {
        return false;
    }
    true
}

fn format_param_value_for_csv(param: &SpsaParam) -> String {
    if param.is_int {
        format!("{}", param.value.round() as i64)
    } else {
        format!("{:.6}", param.value)
    }
}

fn write_stats_csv_header(writer: &mut BufWriter<File>) -> Result<()> {
    writeln!(
        writer,
        "iteration,seed,games,plus_wins,minus_wins,draws,raw_result,active_params,\
         avg_abs_shift,updated_params,avg_abs_update,max_abs_update,total_games"
    )?;
    Ok(())
}

fn write_stats_aggregate_csv_header(writer: &mut BufWriter<File>) -> Result<()> {
    writeln!(
        writer,
        "iteration,seeds,games_per_seed,raw_result_mean,raw_result_variance,\
         plus_wins_mean,plus_wins_variance,minus_wins_mean,minus_wins_variance,draws_mean,draws_variance,total_games"
    )?;
    Ok(())
}

fn write_param_values_csv_header(writer: &mut BufWriter<File>, params: &[SpsaParam]) -> Result<()> {
    write!(writer, "iteration")?;
    for param in params {
        write!(writer, ",{}", param.name)?;
    }
    writeln!(writer)?;
    Ok(())
}

fn open_stats_csv_writer(path: &Path, resume: bool) -> Result<BufWriter<File>> {
    let write_header = if resume {
        if !path.exists() {
            true
        } else {
            std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                == 0
        }
    } else {
        true
    };
    let file = if resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?
    };
    let mut writer = BufWriter::new(file);
    if write_header {
        write_stats_csv_header(&mut writer)?;
        writer.flush()?;
    }
    Ok(writer)
}

fn open_stats_aggregate_csv_writer(path: &Path, resume: bool) -> Result<BufWriter<File>> {
    let write_header = if resume {
        if !path.exists() {
            true
        } else {
            std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                == 0
        }
    } else {
        true
    };
    let file = if resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?
    };
    let mut writer = BufWriter::new(file);
    if write_header {
        write_stats_aggregate_csv_header(&mut writer)?;
        writer.flush()?;
    }
    Ok(writer)
}

fn open_param_values_csv_writer(
    path: &Path,
    resume: bool,
    params: &[SpsaParam],
) -> Result<BufWriter<File>> {
    let write_header = if resume {
        if !path.exists() {
            true
        } else {
            std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                == 0
        }
    } else {
        true
    };
    let file = if resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?
    };
    let mut writer = BufWriter::new(file);
    if write_header {
        write_param_values_csv_header(&mut writer, params)?;
        writer.flush()?;
    }
    Ok(writer)
}

fn write_stats_csv_row(writer: &mut BufWriter<File>, stats: IterationStats) -> Result<()> {
    writeln!(
        writer,
        "{},{},{},{},{},{},{:+.6},{},{:.6},{},{:.6},{:.6},{}",
        stats.iteration,
        stats.seed,
        stats.games,
        stats.plus_wins,
        stats.minus_wins,
        stats.draws,
        stats.raw_result,
        stats.active_params,
        stats.avg_abs_shift,
        stats.updated_params,
        stats.avg_abs_update,
        stats.max_abs_update,
        stats.total_games
    )?;
    Ok(())
}

fn write_stats_aggregate_csv_row(
    writer: &mut BufWriter<File>,
    stats: AggregateIterationStats,
) -> Result<()> {
    writeln!(
        writer,
        "{},{},{},{:+.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{}",
        stats.iteration,
        stats.seed_count,
        stats.games_per_seed,
        stats.raw_result_mean,
        stats.raw_result_variance,
        stats.plus_wins_mean,
        stats.plus_wins_variance,
        stats.minus_wins_mean,
        stats.minus_wins_variance,
        stats.draws_mean,
        stats.draws_variance,
        stats.total_games
    )?;
    Ok(())
}

fn write_param_values_csv_row(
    writer: &mut BufWriter<File>,
    iteration: u32,
    params: &[SpsaParam],
) -> Result<()> {
    write!(writer, "{iteration}")?;
    for param in params {
        write!(writer, ",{}", format_param_value_for_csv(param))?;
    }
    writeln!(writer)?;
    Ok(())
}

fn load_meta(path: &Path) -> Result<ResumeMetaData> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let meta = serde_json::from_reader(reader)
        .with_context(|| format!("failed to parse JSON {}", path.display()))?;
    Ok(meta)
}

fn save_meta(path: &Path, meta: &ResumeMetaData) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, meta)
        .with_context(|| format!("failed to write JSON {}", path.display()))?;
    Ok(())
}

fn parse_param_line(line: &str, line_no: usize) -> Result<Option<SpsaParam>> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(None);
    }

    // 先にコメント (`//` 以降) を切り離し、値部分にだけ `[[NOT USED]]` 判定を適用する。
    // 順序を逆にすると `// 旧: [[NOT USED]]` のようなコメント内のマーカーまで消えて
    // not_used が偽陽性になる。
    let (raw_val_part, comment) = if let Some((left, right)) = trimmed.split_once("//") {
        (left, right.trim().to_string())
    } else {
        (trimmed, String::new())
    };
    let not_used = raw_val_part.contains(PARAM_NOT_USED_MARKER);
    let val_owned: String;
    let val_part: &str = if not_used {
        val_owned = raw_val_part.replace(PARAM_NOT_USED_MARKER, "");
        val_owned.as_str()
    } else {
        raw_val_part
    };

    let cols: Vec<&str> = val_part.split(',').map(str::trim).collect();
    if cols.len() < 7 {
        bail!("invalid params line {}: '{}'", line_no, line);
    }

    let type_name = cols[1].to_string();
    let is_int = type_name.eq_ignore_ascii_case("int");

    Ok(Some(SpsaParam {
        name: cols[0].to_string(),
        type_name,
        is_int,
        value: cols[2]
            .parse::<f64>()
            .with_context(|| format!("invalid v at line {}", line_no))?,
        min: cols[3]
            .parse::<f64>()
            .with_context(|| format!("invalid min at line {}", line_no))?,
        max: cols[4]
            .parse::<f64>()
            .with_context(|| format!("invalid max at line {}", line_no))?,
        c_end: cols[5]
            .parse::<f64>()
            .with_context(|| format!("invalid c_end at line {}", line_no))?,
        r_end: cols[6]
            .parse::<f64>()
            .with_context(|| format!("invalid r_end at line {}", line_no))?,
        comment,
        not_used,
    }))
}

fn read_params(path: &Path) -> Result<Vec<SpsaParam>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut params = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if let Some(param) = parse_param_line(&line, idx + 1)? {
            params.push(param);
        }
    }
    if params.is_empty() {
        bail!("no parameters loaded from {}", path.display());
    }
    Ok(params)
}

fn write_params(path: &Path, params: &[SpsaParam]) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let mut w = BufWriter::new(file);
    for p in params {
        let v_str = if p.is_int {
            format!("{}", p.value.round() as i64)
        } else {
            format!("{}", p.value)
        };
        let mut line = format!(
            "{},{},{},{},{},{},{}",
            p.name, p.type_name, v_str, p.min, p.max, p.c_end, p.r_end
        );
        if !p.comment.is_empty() {
            line.push_str(" //");
            line.push_str(&p.comment);
        }
        if p.not_used {
            line.push_str(PARAM_NOT_USED_MARKER);
        }
        writeln!(w, "{line}")?;
    }
    w.flush()?;
    Ok(())
}

fn option_value_string(param: &SpsaParam, value: f64) -> String {
    if param.is_int {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.6}")
    }
}

fn clamped_value(param: &SpsaParam, raw: f64) -> f64 {
    raw.clamp(param.min, param.max)
}

fn resolve_engine_path(cli: &Cli) -> Result<PathBuf> {
    if let Some(path) = &cli.engine_path {
        return Ok(path.clone());
    }
    let release = PathBuf::from("target/release/rshogi-usi");
    if release.exists() {
        return Ok(release);
    }
    let debug = PathBuf::from("target/debug/rshogi-usi");
    if debug.exists() {
        return Ok(debug);
    }
    bail!("engine binary not found. specify --engine-path or build target/release/rshogi-usi");
}

fn apply_parameter_vector(
    engine: &mut EngineProcess,
    params: &[SpsaParam],
    values: &[f64],
    translator: &EngineNameTranslator,
) -> Result<()> {
    for (p, &v) in params.iter().zip(values.iter()) {
        let (engine_name, engine_value) = translator.translate(&p.name, v);
        engine.set_option_if_available(engine_name, &option_value_string(p, engine_value))?;
    }
    engine.sync_ready()?;
    Ok(())
}

fn plus_score_from_outcome(outcome: GameOutcome, plus_is_black: bool) -> f64 {
    match outcome {
        GameOutcome::Draw | GameOutcome::InProgress => 0.0,
        GameOutcome::BlackWin => {
            if plus_is_black {
                1.0
            } else {
                -1.0
            }
        }
        GameOutcome::WhiteWin => {
            if plus_is_black {
                -1.0
            } else {
                1.0
            }
        }
    }
}

fn pick_startpos_index(
    start_positions_len: usize,
    rng: &mut impl rand::Rng,
    random: bool,
    game_index: usize,
) -> Result<usize> {
    if start_positions_len == 0 {
        bail!("no start positions available");
    }
    if random {
        Ok(rng.random_range(0..start_positions_len))
    } else {
        Ok(game_index % start_positions_len)
    }
}

fn resolve_seeds(cli: &Cli) -> Vec<u64> {
    if let Some(seeds) = &cli.seeds {
        return seeds.clone();
    }
    if let Some(seed) = cli.seed {
        return vec![seed];
    }
    let mut rng = rand::rng();
    vec![rng.random()]
}

fn mean_and_variance(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mean = values.iter().copied().sum::<f64>() / values.len() as f64;
    let variance = values
        .iter()
        .map(|value| {
            let diff = value - mean;
            diff * diff
        })
        .sum::<f64>()
        / values.len() as f64;
    (mean, variance)
}

fn seed_for_iteration(base_seed: u64, iteration_index: u32) -> u64 {
    let iter_term = (iteration_index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    base_seed ^ iter_term
}

fn duplicate_engine_config(cfg: &EngineConfig) -> EngineConfig {
    EngineConfig {
        path: cfg.path.clone(),
        args: cfg.args.clone(),
        threads: cfg.threads,
        hash_mb: cfg.hash_mb,
        network_delay: cfg.network_delay,
        network_delay2: cfg.network_delay2,
        minimum_thinking_time: cfg.minimum_thinking_time,
        slowmover: cfg.slowmover,
        ponder: cfg.ponder,
        usi_options: cfg.usi_options.clone(),
    }
}

fn run_seed_games_parallel(ctx: SeedRunContext<'_>) -> Result<SeedGameStats> {
    let SeedRunContext {
        concurrency,
        base_cfg,
        params,
        plus_values,
        minus_values,
        start_positions,
        start_pos_indices,
        game_cfg,
        tc,
        total_games_start,
        iteration,
        seed_idx,
        seed_count,
        base_seed,
        translator,
    } = ctx;

    let game_count = start_pos_indices.len();
    if game_count == 0 {
        return Ok(SeedGameStats {
            step_sum: 0.0,
            plus_wins: 0,
            minus_wins: 0,
            draws: 0,
        });
    }
    let worker_count = concurrency.clamp(1, game_count);
    let (task_tx, task_rx) = unbounded::<GameTask>();
    let (result_tx, result_rx) = unbounded::<Result<GameTaskResult>>();

    std::thread::scope(|scope| -> Result<SeedGameStats> {
        for worker_idx in 0..worker_count {
            let task_rx = task_rx.clone();
            let result_tx = result_tx.clone();
            let worker_cfg = duplicate_engine_config(base_cfg);
            let worker_label = format!("seed{}_worker{}", seed_idx + 1, worker_idx + 1);
            scope.spawn(move || {
                let mut plus_engine =
                    match EngineProcess::spawn(&worker_cfg, format!("plus_{worker_label}")) {
                        Ok(engine) => engine,
                        Err(err) => {
                            let _ = result_tx.send(Err(err));
                            return;
                        }
                    };
                let mut minus_engine =
                    match EngineProcess::spawn(&worker_cfg, format!("minus_{worker_label}")) {
                        Ok(engine) => engine,
                        Err(err) => {
                            let _ = result_tx.send(Err(err));
                            return;
                        }
                    };
                for task in task_rx {
                    let result = (|| -> Result<GameTaskResult> {
                        if task.plus_is_black {
                            apply_parameter_vector(
                                &mut plus_engine,
                                params,
                                plus_values,
                                translator,
                            )?;
                            apply_parameter_vector(
                                &mut minus_engine,
                                params,
                                minus_values,
                                translator,
                            )?;
                        } else {
                            apply_parameter_vector(
                                &mut plus_engine,
                                params,
                                minus_values,
                                translator,
                            )?;
                            apply_parameter_vector(
                                &mut minus_engine,
                                params,
                                plus_values,
                                translator,
                            )?;
                        }
                        plus_engine.new_game()?;
                        minus_engine.new_game()?;

                        let start_pos = &start_positions[task.start_pos_index];
                        let mut on_move = |_event: &MoveEvent| {};
                        let result = if task.plus_is_black {
                            run_game(
                                &mut plus_engine,
                                &mut minus_engine,
                                start_pos,
                                tc,
                                game_cfg,
                                task.game_id,
                                &mut on_move,
                                None,
                            )?
                        } else {
                            run_game(
                                &mut minus_engine,
                                &mut plus_engine,
                                start_pos,
                                tc,
                                game_cfg,
                                task.game_id,
                                &mut on_move,
                                None,
                            )?
                        };
                        let plus_score =
                            plus_score_from_outcome(result.outcome, task.plus_is_black);
                        Ok(GameTaskResult {
                            game_idx: task.game_idx,
                            plus_is_black: task.plus_is_black,
                            plus_score,
                            outcome: result.outcome,
                        })
                    })();
                    if result_tx.send(result).is_err() {
                        break;
                    }
                }
            });
        }
        drop(task_rx);
        drop(result_tx);

        for (idx, &start_pos_index) in start_pos_indices.iter().enumerate() {
            let game_idx = u32::try_from(idx).context("game index overflow")?;
            let game_id = u32::try_from(total_games_start + idx + 1).context("game id overflow")?;
            task_tx
                .send(GameTask {
                    game_idx,
                    plus_is_black: idx % 2 == 0,
                    start_pos_index,
                    game_id,
                })
                .context("failed to dispatch game task")?;
        }
        drop(task_tx);

        let mut step_sum = 0.0f64;
        let mut plus_wins = 0u32;
        let mut minus_wins = 0u32;
        let mut draws = 0u32;

        for _ in 0..game_count {
            let result =
                result_rx.recv().context("failed to receive game result from worker")??;
            step_sum += result.plus_score;
            if result.plus_score > 0.0 {
                plus_wins += 1;
            } else if result.plus_score < 0.0 {
                minus_wins += 1;
            } else {
                draws += 1;
            }
            println!(
                "iter={} seed={}/{}({}) game={}/{} plus_is_black={} outcome={} plus_score={:+.1}",
                iteration,
                seed_idx + 1,
                seed_count,
                base_seed,
                result.game_idx + 1,
                game_count,
                result.plus_is_black,
                result.outcome.label(),
                result.plus_score
            );
        }

        Ok(SeedGameStats {
            step_sum,
            plus_wins,
            minus_wins,
            draws,
        })
    })
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();

    let cli = Cli::parse();
    if cli.games_per_iteration == 0 || cli.games_per_iteration % 2 != 0 {
        bail!("--games-per-iteration must be an even number >= 2");
    }
    if cli.iterations == 0 {
        bail!("--iterations must be >= 1");
    }
    if cli.concurrency == 0 {
        bail!("--concurrency must be >= 1");
    }
    if cli.alpha <= 0.0 || cli.gamma <= 0.0 {
        bail!("--alpha and --gamma must be > 0");
    }
    if cli.a_ratio < 0.0 {
        bail!("--a-ratio must be >= 0");
    }
    if let Some(v) = cli.early_stop_avg_abs_update_threshold
        && v < 0.0
    {
        bail!("--early-stop-avg-abs-update-threshold must be >= 0");
    }
    if let Some(v) = cli.early_stop_result_variance_threshold
        && v < 0.0
    {
        bail!("--early-stop-result-variance-threshold must be >= 0");
    }
    let early_stop_config = match (
        cli.early_stop_avg_abs_update_threshold,
        cli.early_stop_result_variance_threshold,
        cli.early_stop_patience,
    ) {
        (None, None, 0) => None,
        (Some(avg), Some(var), patience) if patience > 0 => Some(EarlyStopConfig {
            avg_abs_update_threshold: avg,
            result_variance_threshold: var,
            patience,
        }),
        _ => {
            bail!(
                "early stopを有効化するには \
                 --early-stop-avg-abs-update-threshold, \
                 --early-stop-result-variance-threshold, \
                 --early-stop-patience(>0) を全て指定してください"
            );
        }
    };

    let active_only_regex = cli
        .active_only_regex
        .as_deref()
        .map(Regex::new)
        .transpose()
        .context("invalid --active-only-regex")?;
    let seed_values = resolve_seeds(&cli);
    if seed_values.is_empty() {
        bail!("at least one seed is required");
    }
    println!("using base seeds: {:?}", seed_values);

    let engine_path = resolve_engine_path(&cli)?;
    let engine_args = cli.engine_args.clone().unwrap_or_default();
    if let Some(init_src) = &cli.init_from {
        if let Some(parent) = cli.params.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent dir for {}", cli.params.display())
            })?;
        }
        // TOCTOU 緩和: `exists()` チェック→ `fs::copy` の間に他プロセスが書き込むと
        // 上書きしてしまう。`OpenOptions::create_new` で atomic に作成失敗 (AlreadyExists)
        // を検出し、その場合は既存ファイルを尊重する。
        match OpenOptions::new().create_new(true).write(true).open(&cli.params) {
            Ok(out) => {
                let mut writer = BufWriter::new(out);
                let mut reader = File::open(init_src)
                    .with_context(|| format!("failed to open {}", init_src.display()))?;
                std::io::copy(&mut reader, &mut writer).with_context(|| {
                    format!("failed to copy {} -> {}", init_src.display(), cli.params.display())
                })?;
                writer.flush()?;
                println!(
                    "initialized {} from canonical {}",
                    cli.params.display(),
                    init_src.display()
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                println!(
                    "init-from: {} already exists, leaving as-is (canonical {} not copied)",
                    cli.params.display(),
                    init_src.display()
                );
            }
            Err(e) => {
                return Err(anyhow::anyhow!(e)
                    .context(format!("failed to create {} for init-from", cli.params.display())));
            }
        }
    }
    let translator = match &cli.engine_param_mapping {
        Some(path) => {
            let t = EngineNameTranslator::from_mapping_file(path)?;
            println!("engine param mapping: {} entries loaded from {}", t.len(), path.display());
            t
        }
        None => EngineNameTranslator::empty(),
    };
    let mut params = read_params(&cli.params)?;
    let schedule = ScheduleConfig {
        alpha: cli.alpha,
        gamma: cli.gamma,
        a_ratio: cli.a_ratio,
        mobility: cli.mobility,
        total_iterations: cli.iterations,
    };
    let meta_path = cli.meta_file.clone().unwrap_or_else(|| default_meta_path(&cli.params));
    let (start_iteration, mut total_games) = if cli.resume {
        let meta = load_meta(&meta_path).with_context(|| {
            format!("--resume was set but metadata load failed: {}", meta_path.display())
        })?;
        if meta.format_version != META_FORMAT_VERSION {
            bail!(
                "unsupported meta format version {} in {}",
                meta.format_version,
                meta_path.display()
            );
        }
        if !schedule_matches(meta.schedule, schedule) {
            if cli.force_schedule {
                eprintln!(
                    "warning: schedule differs from metadata but continuing due to --force-schedule \
                     (meta={}, meta_schedule={:?}, cli_schedule={:?})",
                    meta_path.display(),
                    meta.schedule,
                    schedule
                );
            } else {
                bail!(
                    "schedule mismatch with {}. use --force-schedule to override \
                     (meta_schedule={:?}, cli_schedule={:?})",
                    meta_path.display(),
                    meta.schedule,
                    schedule
                );
            }
        }
        (meta.completed_iterations, meta.total_games)
    } else {
        (0, 0)
    };
    let end_iteration = start_iteration
        .checked_add(cli.iterations)
        .context("iteration index overflow")?;
    let stats_csv_path: Option<PathBuf> = if cli.no_stats_csv {
        None
    } else {
        Some(cli.stats_csv.clone().unwrap_or_else(|| default_stats_csv_path(&cli.params)))
    };
    let aggregate_csv_path: Option<PathBuf> = if cli.no_stats_aggregate_csv {
        None
    } else if let Some(path) = &cli.stats_aggregate_csv {
        Some(path.clone())
    } else if seed_values.len() > 1 {
        // 互換性: --stats-csv が明示指定されている場合は従来の派生
        // (<stats_csv>.aggregate.csv) を維持。さもなければ <params>.stats_aggregate.csv。
        // これにより既存ジョブを --resume したとき既定の集計CSV出力先が変わらない。
        if let Some(stats_path) = &cli.stats_csv {
            Some(PathBuf::from(format!("{}.aggregate.csv", stats_path.display())))
        } else {
            Some(default_stats_aggregate_csv_path(&cli.params))
        }
    } else {
        None
    };
    let mut stats_csv_writer = if let Some(path) = stats_csv_path.as_deref() {
        Some(open_stats_csv_writer(path, cli.resume)?)
    } else {
        None
    };
    let mut stats_aggregate_csv_writer = if let Some(path) = aggregate_csv_path.as_deref() {
        Some(open_stats_aggregate_csv_writer(path, cli.resume)?)
    } else {
        None
    };
    let param_values_csv_path: Option<PathBuf> = if cli.no_param_values_csv {
        None
    } else {
        Some(
            cli.param_values_csv
                .clone()
                .unwrap_or_else(|| default_param_values_csv_path(&cli.params)),
        )
    };
    let mut param_values_csv_writer = if let Some(path) = param_values_csv_path.as_deref() {
        Some(open_param_values_csv_writer(path, cli.resume, &params)?)
    } else {
        None
    };

    if cli.startpos_file.is_none() {
        if cli.require_startpos_file {
            bail!("--require-startpos-file was set but --startpos-file was not provided");
        }
        eprintln!(
            "warning: --startpos-file is not specified. opening diversity may be insufficient"
        );
    }

    let (start_positions, _) =
        load_start_positions(cli.startpos_file.as_deref(), cli.sfen.as_deref(), None, None)?;
    let active_param_count = params
        .iter()
        .filter(|param| is_param_active(param, active_only_regex.as_ref(), &translator))
        .count();
    if active_param_count == 0 {
        bail!(
            "no active parameters (active_only_regex={:?}, not_used filtering may have excluded all)",
            cli.active_only_regex
        );
    }
    println!("active params: {active_param_count}/{}", params.len());

    // 翻訳器有効時、`active_only_regex` でマッチしたが unmapped で除外されたパラメータを
    // info 出力する。「期待した parameter が摂動されていない」事象に気づきやすくする。
    if translator.is_enabled() {
        let mut unmapped_active: Vec<&str> = params
            .iter()
            .filter(|p| {
                !p.not_used
                    && active_only_regex.as_ref().is_none_or(|re| re.is_match(&p.name))
                    && !translator.is_mapped(&p.name)
            })
            .map(|p| p.name.as_str())
            .collect();
        if !unmapped_active.is_empty() {
            unmapped_active.sort();
            println!(
                "info: {} param(s) matched --active-only-regex but are unmapped (translator skipped):",
                unmapped_active.len()
            );
            for n in &unmapped_active {
                println!("  - {n}");
            }
        }
    }

    let base_cfg = EngineConfig {
        path: engine_path,
        args: engine_args,
        threads: cli.threads,
        hash_mb: cli.hash_mb,
        network_delay: None,
        network_delay2: None,
        minimum_thinking_time: None,
        slowmover: None,
        ponder: false,
        usi_options: cli.usi_options.clone().unwrap_or_default(),
    };

    let game_cfg = GameConfig {
        max_moves: cli.max_moves,
        timeout_margin_ms: cli.timeout_margin_ms,
        pass_rights: None,
        go_depth: None,
        go_nodes: cli.nodes,
    };
    let tc = if cli.nodes.is_some() {
        // ノード数指定時は時間制御不要だが、タイムアウト検出用に十分大きな値を設定
        TimeControl::new(0, 0, 0, 0, 0)
    } else if let Some(btime) = cli.btime {
        TimeControl::new(btime, btime, cli.binc, cli.binc, 0)
    } else {
        TimeControl::new(0, 0, 0, 0, cli.byoyomi)
    };
    let mut early_stop_consecutive = 0u32;

    // Fishtest 方式: per-param スケジュール定数を初期化
    let big_a = schedule.a_ratio * end_iteration as f64;
    let param_schedules: Vec<ParamScheduleConstants> = params
        .iter()
        .map(|p| {
            ParamScheduleConstants::compute(
                p.c_end,
                p.r_end,
                end_iteration,
                schedule.a_ratio,
                schedule.alpha,
                schedule.gamma,
            )
        })
        .collect();

    for iter in start_iteration..end_iteration {
        let mut update_sums = vec![0.0f64; params.len()];
        let mut seed_raw_results = Vec::with_capacity(seed_values.len());
        let mut seed_plus_wins = Vec::with_capacity(seed_values.len());
        let mut seed_minus_wins = Vec::with_capacity(seed_values.len());
        let mut seed_draws = Vec::with_capacity(seed_values.len());
        let mut seed_rows = Vec::with_capacity(seed_values.len());

        for (seed_idx, base_seed) in seed_values.iter().copied().enumerate() {
            let iter_seed = seed_for_iteration(base_seed, iter);
            let mut rng = ChaCha8Rng::seed_from_u64(iter_seed);

            // Per-param Fishtest 摂動: shift_j = c_k_j × flip_j
            let flips: Vec<f64> = params
                .iter()
                .map(|p| {
                    if !is_param_active(p, active_only_regex.as_ref(), &translator) {
                        0.0
                    } else if rng.random_bool(0.5) {
                        1.0
                    } else {
                        -1.0
                    }
                })
                .collect();
            let shifts: Vec<f64> = params
                .iter()
                .zip(param_schedules.iter())
                .zip(flips.iter())
                .map(|((p, sched), &flip)| {
                    if !is_param_active(p, active_only_regex.as_ref(), &translator) {
                        0.0
                    } else {
                        let (c_k, _) =
                            sched.at_iteration(iter, big_a, schedule.alpha, schedule.gamma);
                        c_k * flip
                    }
                })
                .collect();

            let plus_values: Vec<f64> = params
                .iter()
                .zip(shifts.iter())
                .map(|(p, s)| clamped_value(p, p.value + s))
                .collect();
            let minus_values: Vec<f64> = params
                .iter()
                .zip(shifts.iter())
                .map(|(p, s)| clamped_value(p, p.value - s))
                .collect();

            let mut active_params = 0usize;
            let mut abs_shift_sum = 0.0f64;
            for (p, &shift) in params.iter().zip(shifts.iter()) {
                if !is_param_active(p, active_only_regex.as_ref(), &translator) {
                    continue;
                }
                active_params += 1;
                abs_shift_sum += shift.abs();
            }
            let avg_abs_shift = if active_params > 0 {
                abs_shift_sum / active_params as f64
            } else {
                0.0
            };
            let seed_total_games_start = total_games;
            let mut start_pos_indices = Vec::with_capacity(cli.games_per_iteration as usize);
            for game_idx in 0..cli.games_per_iteration as usize {
                start_pos_indices.push(pick_startpos_index(
                    start_positions.len(),
                    &mut rng,
                    cli.random_startpos,
                    seed_total_games_start + game_idx,
                )?);
            }
            let seed_game_stats = run_seed_games_parallel(SeedRunContext {
                concurrency: cli.concurrency,
                base_cfg: &base_cfg,
                params: &params,
                plus_values: &plus_values,
                minus_values: &minus_values,
                start_positions: &start_positions,
                start_pos_indices: &start_pos_indices,
                game_cfg: &game_cfg,
                tc,
                total_games_start: seed_total_games_start,
                iteration: iter + 1,
                seed_idx,
                seed_count: seed_values.len(),
                base_seed,
                translator: &translator,
            })?;
            total_games = total_games
                .checked_add(cli.games_per_iteration as usize)
                .context("total_games overflow")?;
            let step_sum = seed_game_stats.step_sum;
            let plus_wins = seed_game_stats.plus_wins;
            let minus_wins = seed_game_stats.minus_wins;
            let draws = seed_game_stats.draws;

            // Fishtest 更新: signal_j = R_k_j × c_k_j × result × flip_j
            let raw_result = step_sum;
            for (idx, (p, (&flip, sched))) in
                params.iter().zip(flips.iter().zip(param_schedules.iter())).enumerate()
            {
                if !is_param_active(p, active_only_regex.as_ref(), &translator)
                    || p.c_end.abs() <= f64::EPSILON
                {
                    continue;
                }
                let (c_k, r_k) = sched.at_iteration(iter, big_a, schedule.alpha, schedule.gamma);
                update_sums[idx] += r_k * c_k * raw_result * flip;
            }

            seed_raw_results.push(raw_result);
            seed_plus_wins.push(plus_wins as f64);
            seed_minus_wins.push(minus_wins as f64);
            seed_draws.push(draws as f64);

            seed_rows.push(IterationStats {
                iteration: iter + 1,
                seed: base_seed,
                games: cli.games_per_iteration,
                plus_wins,
                minus_wins,
                draws,
                raw_result,
                active_params,
                avg_abs_shift,
                updated_params: 0,
                avg_abs_update: 0.0,
                max_abs_update: 0.0,
                total_games: 0,
            });
        }

        // Seed 平均後にパラメータ更新
        let mut updated_params = 0usize;
        let mut abs_update_sum = 0.0f64;
        let mut max_abs_update = 0.0f64;
        for (idx, p) in params.iter_mut().enumerate() {
            if !is_param_active(p, active_only_regex.as_ref(), &translator)
                || p.c_end.abs() <= f64::EPSILON
            {
                continue;
            }
            let before = p.value;
            let avg_signal = update_sums[idx] / seed_values.len() as f64;
            let updated = clamped_value(p, p.value + avg_signal * cli.mobility);
            p.value = if p.is_int { updated.round() } else { updated };
            let abs_update = (p.value - before).abs();
            updated_params += 1;
            abs_update_sum += abs_update;
            if abs_update > max_abs_update {
                max_abs_update = abs_update;
            }
        }
        let avg_abs_update = if updated_params > 0 {
            abs_update_sum / updated_params as f64
        } else {
            0.0
        };
        if let Some(writer) = stats_csv_writer.as_mut() {
            for row in &mut seed_rows {
                row.updated_params = updated_params;
                row.avg_abs_update = avg_abs_update;
                row.max_abs_update = max_abs_update;
                row.total_games = total_games;
                write_stats_csv_row(writer, *row)?;
            }
            writer.flush()?;
        }

        let (raw_result_mean, raw_result_variance) = mean_and_variance(&seed_raw_results);
        let (plus_wins_mean, plus_wins_variance) = mean_and_variance(&seed_plus_wins);
        let (minus_wins_mean, minus_wins_variance) = mean_and_variance(&seed_minus_wins);
        let (draws_mean, draws_variance) = mean_and_variance(&seed_draws);

        write_params(&cli.params, &params)?;
        if let Some(writer) = param_values_csv_writer.as_mut() {
            write_param_values_csv_row(writer, iter + 1, &params)?;
            writer.flush()?;
        }
        let meta = ResumeMetaData {
            format_version: META_FORMAT_VERSION,
            params_file: cli.params.display().to_string(),
            completed_iterations: iter + 1,
            total_games,
            last_raw_result_mean: raw_result_mean,
            last_avg_abs_update: avg_abs_update,
            updated_at_utc: Utc::now().to_rfc3339(),
            schedule,
        };
        save_meta(&meta_path, &meta)?;
        println!(
            "iter={} seeds={} raw_result_mean={:+.3} raw_result_var={:.6} \
             avg_abs_update={:.6} max_abs_update={:.6} checkpoint={} meta={}",
            iter + 1,
            seed_values.len(),
            raw_result_mean,
            raw_result_variance,
            avg_abs_update,
            max_abs_update,
            cli.params.display(),
            meta_path.display()
        );
        if let Some(writer) = stats_aggregate_csv_writer.as_mut() {
            write_stats_aggregate_csv_row(
                writer,
                AggregateIterationStats {
                    iteration: iter + 1,
                    seed_count: seed_values.len(),
                    games_per_seed: cli.games_per_iteration,
                    raw_result_mean,
                    raw_result_variance,
                    plus_wins_mean,
                    plus_wins_variance,
                    minus_wins_mean,
                    minus_wins_variance,
                    draws_mean,
                    draws_variance,
                    total_games,
                },
            )?;
            writer.flush()?;
        }

        if let Some(config) = early_stop_config {
            let early_stop_hit = avg_abs_update <= config.avg_abs_update_threshold
                && raw_result_variance <= config.result_variance_threshold;
            if early_stop_hit {
                early_stop_consecutive = early_stop_consecutive.saturating_add(1);
            } else {
                early_stop_consecutive = 0;
            }
            println!(
                "iter={} early_stop_hit={} consecutive={}/{} thresholds(avg_abs_update<={:.6}, result_variance<={:.6})",
                iter + 1,
                early_stop_hit,
                early_stop_consecutive,
                config.patience,
                config.avg_abs_update_threshold,
                config.result_variance_threshold
            );
            if early_stop_consecutive >= config.patience {
                println!(
                    "early stop triggered at iter={} (consecutive={})",
                    iter + 1,
                    early_stop_consecutive
                );
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_at_final_iteration_matches_end_values() {
        let c_end = 50.0;
        let r_end = 0.002;
        let n = 200u32;
        let a_ratio = 0.1;
        let alpha = 0.602;
        let gamma = 0.101;
        let big_a = a_ratio * n as f64;

        let sched = ParamScheduleConstants::compute(c_end, r_end, n, a_ratio, alpha, gamma);
        let (c_k, r_k) = sched.at_iteration(n - 1, big_a, alpha, gamma);

        assert!(
            (c_k - c_end).abs() < 1e-6,
            "c_k at final iter should equal c_end: got {c_k}, expected {c_end}"
        );
        assert!(
            (r_k - r_end).abs() < 1e-6,
            "R_k at final iter should equal r_end: got {r_k}, expected {r_end}"
        );
    }

    #[test]
    fn update_magnitude_is_nonzero_for_typical_params() {
        let c_end = 50.0;
        let r_end = 0.002;
        let n = 200u32;
        let a_ratio = 0.1;
        let alpha = 0.602;
        let gamma = 0.101;
        let big_a = a_ratio * n as f64;

        let sched = ParamScheduleConstants::compute(c_end, r_end, n, a_ratio, alpha, gamma);

        // 初期イテレーション (iter=0) での更新量
        let (c_k, r_k) = sched.at_iteration(0, big_a, alpha, gamma);
        let result = 8.0; // 64局で期待される |W-L| ≈ √64
        let update = r_k * c_k * result;
        assert!(update.abs() > 0.5, "update at iter 0 should be significant: got {update}");

        // 最終イテレーション (iter=199) での更新量
        let (c_k, r_k) = sched.at_iteration(n - 1, big_a, alpha, gamma);
        let update = r_k * c_k * result;
        assert!(update.abs() > 0.1, "update at final iter should still be nonzero: got {update}");
    }

    #[test]
    fn early_iterations_have_larger_perturbation() {
        let c_end = 50.0;
        let r_end = 0.002;
        let n = 200u32;
        let a_ratio = 0.1;
        let alpha = 0.602;
        let gamma = 0.101;
        let big_a = a_ratio * n as f64;

        let sched = ParamScheduleConstants::compute(c_end, r_end, n, a_ratio, alpha, gamma);
        let (c_0, _) = sched.at_iteration(0, big_a, alpha, gamma);
        let (c_last, _) = sched.at_iteration(n - 1, big_a, alpha, gamma);
        assert!(c_0 > c_last, "c_k should decrease over iterations: c_0={c_0}, c_last={c_last}");
    }
}
