//! Floodgate棋譜取得・変換パイプライン
//!
//! # 使用例
//!
//! ```bash
//! # 0. 高レートプレイヤーリストを取得（ダウンロード事前フィルタ用）
//! cargo run -p tools --bin floodgate_pipeline -- fetch-ratings --min-rating 3900 --out high_rated.txt
//!
//! # 1. インデックスファイルをダウンロード
//! cargo run -p tools --bin floodgate_pipeline -- fetch-index --out 00LIST.floodgate
//!
//! # 2. CSAファイルをダウンロード（日付 + プレイヤーでフィルタ、並列DL）
//! cargo run -p tools --bin floodgate_pipeline -- download --date-from 2026-03-10 --player-file players.txt --concurrency 16
//!
//! # 3. SFENを抽出（レーティングで精密フィルタ、並列パース）
//! cargo run -p tools --bin floodgate_pipeline -- extract --min-rating 3900 --max-ply 32
//! ```

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use rayon::prelude::*;
use reqwest::blocking::Client;
use rshogi_csa::parse_csa;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tools::common::dedup::DedupSet;
use tools::common::floodgate as fg;
use tools::common::io::open_writer;
use tools::common::sfen_ops::{canonicalize_4t_with_mirror, mirror_horizontal};

#[derive(Parser)]
#[command(
    name = "floodgate-pipeline",
    version,
    about = "Floodgate棋譜取得・変換パイプライン\n\nFloodgate → CSA → SFEN → mirror → dedup"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Floodgateレーティングページから高レートプレイヤー名を取得
    FetchRatings {
        /// レーティングページ URL
        #[arg(long, default_value = fg::RATING_PAGE_URL)]
        url: String,
        /// レーティング閾値（この値以上のプレイヤーを出力）
        #[arg(long, default_value_t = 3900)]
        min_rating: u32,
        /// 出力ファイルパス（1行1プレイヤー名）
        #[arg(long, default_value = "high_rated_players.txt")]
        out: String,
    },
    /// 00LIST.floodgateインデックスをダウンロード
    FetchIndex {
        /// Root URL (HTTP only)
        #[arg(long, default_value = fg::DEFAULT_ROOT)]
        root: String,
        /// 出力ファイルパス
        #[arg(long, default_value = "00LIST.floodgate")]
        out: String,
    },
    /// インデックスファイルに記載されたCSAファイルをダウンロード
    Download {
        /// 00LIST.floodgateのパス
        #[arg(long, default_value = "00LIST.floodgate")]
        index: String,
        /// Root URL (HTTP only)
        #[arg(long, default_value = fg::DEFAULT_ROOT)]
        root: String,
        /// 出力ディレクトリ
        #[arg(long, default_value = "logs/x")]
        out_dir: String,
        /// ダウンロード数の上限（テスト用）
        #[arg(long)]
        limit: Option<usize>,
        /// この日付以降のファイルのみダウンロード（YYYY-MM-DD）
        #[arg(long)]
        date_from: Option<String>,
        /// この日付以前のファイルのみダウンロード（YYYY-MM-DD）
        #[arg(long)]
        date_to: Option<String>,
        /// プレイヤー名ファイル（1行1名）。いずれかの対局者がリストに含まれるゲームをDL
        #[arg(long)]
        player_file: Option<String>,
        /// 並列ダウンロード数（0 = CPU コア数に自動設定）
        #[arg(long, default_value_t = 8)]
        concurrency: usize,
    },
    /// ローカルのCSAファイルからSFENを抽出
    Extract {
        /// CSAファイルが格納されたルートディレクトリ (例: logs/x/2025/01/*.csa)
        #[arg(long, default_value = "logs/x")]
        root: String,
        /// 出力パス ("-" で標準出力; .gz対応)
        #[arg(long, default_value = "sfens.txt")]
        out: String,
        /// 抽出モード
        #[arg(long, value_enum, default_value_t = Mode::All)]
        mode: Mode,
        /// mode=nthの場合、抽出する手数（カンマ区切りで複数指定可）
        #[arg(long, value_delimiter = ',')]
        nth: Vec<u32>,
        /// 水平ミラーで正規化して重複排除
        #[arg(long)]
        mirror_dedup: bool,
        /// 各SFENの水平ミラーも出力（--mirror-dedup=falseの場合のみ有効）
        #[arg(long)]
        emit_mirror: bool,
        /// この手数以上の局面のみ抽出（1=初期局面）
        #[arg(long, default_value_t = 1)]
        min_ply: u32,
        /// この手数以下の局面のみ抽出（0=制限なし）
        #[arg(long, default_value_t = 0)]
        max_ply: u32,
        /// 1棋譜あたりの最大抽出数（0=無制限）。dedup 後の実書き出し数でカウント
        #[arg(long, default_value_t = 0)]
        per_game_cap: usize,
        /// 両対局者のレーティング下限（0=フィルタなし）
        #[arg(long, default_value_t = 0)]
        min_rating: u32,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Mode {
    /// 初期局面のみ
    Initial,
    /// 全局面
    All,
    /// 指定した手数の局面のみ
    Nth,
}

/// extract サブコマンドのオプション
struct ExtractOptions<'a> {
    mode: Mode,
    nth: &'a [u32],
    mirror_dedup: bool,
    emit_mirror: bool,
    min_ply: u32,
    max_ply: u32,
    per_game_cap: usize,
    min_rating: u32,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::FetchRatings {
            url,
            min_rating,
            out,
        } => run_fetch_ratings(&url, min_rating, &out),
        Cmd::FetchIndex { root, out } => run_fetch_index(&root, &out),
        Cmd::Download {
            index,
            root,
            out_dir,
            limit,
            date_from,
            date_to,
            player_file,
            concurrency,
        } => run_download(
            &index,
            &root,
            &out_dir,
            limit,
            date_from.as_deref(),
            date_to.as_deref(),
            player_file.as_deref(),
            concurrency,
        ),
        Cmd::Extract {
            root,
            out,
            mode,
            nth,
            mirror_dedup,
            emit_mirror,
            min_ply,
            max_ply,
            per_game_cap,
            min_rating,
        } => {
            let opts = ExtractOptions {
                mode,
                nth: &nth,
                mirror_dedup,
                emit_mirror,
                min_ply,
                max_ply,
                per_game_cap,
                min_rating,
            };
            run_extract(&root, &out, &opts)
        }
    }
}

fn run_fetch_ratings(url: &str, min_rating: u32, out: &str) -> Result<()> {
    eprintln!("Fetching rating page from: {url}");
    let client = Client::builder().build()?;
    let html = fg::http_get_text(&client, url)?;
    let all = fg::parse_rating_page(&html);
    eprintln!("Found {} players on rating page", all.len());
    let filtered: Vec<_> = all.iter().filter(|(_, r)| *r >= min_rating as f64).collect();
    eprintln!("{} players with rating >= {min_rating}", filtered.len());
    let mut f = fs::File::create(out).with_context(|| format!("create {out}"))?;
    for (name, rating) in &filtered {
        writeln!(f, "{name}\t{rating}")?;
    }
    eprintln!("Wrote player list to: {out}");
    for (name, rating) in &filtered {
        eprintln!("  {rating:.0}\t{name}");
    }
    Ok(())
}

fn run_fetch_index(root: &str, out: &str) -> Result<()> {
    let url = fg::join_url(root, "00LIST.floodgate")?;
    eprintln!("Fetching index from: {url}");
    let client = Client::builder().build()?;
    let text = fg::http_get_text(&client, &url)?;
    fs::write(out, text).with_context(|| format!("write index: {out}"))?;
    eprintln!("Wrote index to: {out}");
    Ok(())
}

/// パスから日付を YYYYMMDD 形式の整数で抽出。
fn date_of_path(rel: &str) -> Option<u32> {
    if rel.len() < 10 {
        return None;
    }
    let y: u32 = rel.get(..4)?.parse().ok()?;
    let m: u32 = rel.get(5..7)?.parse().ok()?;
    let d: u32 = rel.get(8..10)?.parse().ok()?;
    Some(y * 10000 + m * 100 + d)
}

fn parse_date_arg(s: &str) -> Result<u32> {
    let parts: Vec<&str> = s.split('-').collect();
    anyhow::ensure!(parts.len() == 3, "日付は YYYY-MM-DD 形式で指定してください: {s}");
    let y: u32 = parts[0].parse().with_context(|| format!("年の解析に失敗: {s}"))?;
    let m: u32 = parts[1].parse().with_context(|| format!("月の解析に失敗: {s}"))?;
    let d: u32 = parts[2].parse().with_context(|| format!("日の解析に失敗: {s}"))?;
    anyhow::ensure!((1..=12).contains(&m) && (1..=31).contains(&d), "無効な日付: {s}");
    Ok(y * 10000 + m * 100 + d)
}

fn run_download(
    index: &str,
    root: &str,
    out_dir: &str,
    limit: Option<usize>,
    date_from: Option<&str>,
    date_to: Option<&str>,
    player_file: Option<&str>,
    concurrency: usize,
) -> Result<()> {
    let r = tools::common::io::open_reader(index)?;
    let all_lines = fg::parse_index_lines(r)?;
    let total = all_lines.len();

    let date_from = date_from.map(parse_date_arg).transpose()?;
    let date_to = date_to.map(parse_date_arg).transpose()?;

    let player_patterns = if let Some(pf) = player_file {
        let patterns = fg::load_player_patterns(Path::new(pf))?;
        eprintln!("Loaded {} player patterns from {pf}", patterns.len());
        Some(patterns)
    } else {
        None
    };

    let lines: Vec<String> = all_lines
        .into_iter()
        .filter(|rel| {
            let date = date_of_path(rel).unwrap_or(0);
            if date_from.is_some_and(|df| date < df) || date_to.is_some_and(|dt| date > dt) {
                return false;
            }
            if let Some(ref patterns) = player_patterns {
                if let Some((a, b)) = fg::players_from_path(rel) {
                    fg::player_matches(a, patterns) || fg::player_matches(b, patterns)
                } else {
                    false
                }
            } else {
                true
            }
        })
        .collect();

    let after_filter = lines.len();
    let count = limit.unwrap_or(after_filter).min(after_filter);
    eprintln!(
        "Downloading {} CSA files (total in index: {}, after filter: {}, concurrency: {})",
        count, total, after_filter, concurrency
    );

    let out_dir_path = Path::new(out_dir);
    let to_download: Vec<&str> = lines
        .iter()
        .take(count)
        .filter(|rel| !fg::local_path_for(out_dir_path, rel).exists())
        .map(|s| s.as_str())
        .collect();
    let skipped = count - to_download.len();
    eprintln!("{} files to download ({skipped} already exist)", to_download.len());

    if to_download.is_empty() {
        eprintln!("Download complete. 0 new, {skipped} already existed. Dir: {out_dir}");
        return Ok(());
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(concurrency)
        .build()
        .context("Failed to create thread pool")?;

    let downloaded = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);

    pool.install(|| {
        // thread_local! で Client を再利用し TCP コネクションプールの恩恵を得る
        thread_local! {
            static CLIENT: Client = Client::builder().build().expect("reqwest client");
        }

        to_download.par_iter().for_each(|rel| {
            let url = match fg::join_url(root, rel) {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("  Warning: invalid URL for {rel}: {e}");
                    errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            let out_path = fg::local_path_for(out_dir_path, rel);
            CLIENT.with(|client| match fg::http_get_to_file_noclobber(client, &url, &out_path) {
                Ok(_) => {
                    let n = downloaded.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(500) {
                        eprintln!("  Downloaded {n} new files...");
                    }
                }
                Err(e) => {
                    eprintln!("  Warning: failed to download {rel}: {e}");
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            });
        });
    });

    let dl = downloaded.load(Ordering::Relaxed);
    let err = errors.load(Ordering::Relaxed);
    eprintln!("Download complete. {dl} new, {skipped} already existed. Dir: {out_dir}");
    if err > 0 {
        eprintln!("  ({err} download errors)");
    }
    Ok(())
}

fn visit_csa_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let p = entry.path();
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && ext.eq_ignore_ascii_case("csa")
            {
                files.push(p.to_path_buf());
            }
        }
    }
    files.sort();
    Ok(files)
}

/// 1棋譜から抽出した SFEN のリスト
struct GameResult {
    sfens: Vec<String>,
    error: bool,
    rating_skipped: bool,
    no_rating: bool,
}

/// 1棋譜の CSA パース → SFEN 抽出（純粋関数、副作用なし）
///
/// per_game_cap は dedup 前の上限（dedup 後のカウントは呼び出し側で行う）
fn extract_sfens_from_game(path: &Path, opts: &ExtractOptions) -> GameResult {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("Failed to read {}: {e}", path.display());
            return GameResult {
                sfens: Vec::new(),
                error: true,
                rating_skipped: false,
                no_rating: false,
            };
        }
    };
    let (mut pos, moves, info) = match parse_csa(&text) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("Failed to parse {}: {e}", path.display());
            return GameResult {
                sfens: Vec::new(),
                error: true,
                rating_skipped: false,
                no_rating: false,
            };
        }
    };

    if opts.min_rating > 0 {
        if info.black_rating.is_none() || info.white_rating.is_none() {
            return GameResult {
                sfens: Vec::new(),
                error: false,
                rating_skipped: false,
                no_rating: true,
            };
        }
        if !info.both_ratings_at_least(opts.min_rating as f64) {
            return GameResult {
                sfens: Vec::new(),
                error: false,
                rating_skipped: true,
                no_rating: false,
            };
        }
    }

    let mut sfens = Vec::new();

    match opts.mode {
        Mode::Initial => {
            if in_ply_range(1, opts.min_ply, opts.max_ply) {
                collect_sfen(&pos.to_sfen(), opts.mirror_dedup, opts.emit_mirror, &mut sfens);
            }
        }
        Mode::All => {
            if in_ply_range(1, opts.min_ply, opts.max_ply) {
                collect_sfen(&pos.to_sfen(), opts.mirror_dedup, opts.emit_mirror, &mut sfens);
            }
            for (i, m) in moves.iter().enumerate() {
                if pos.apply_csa_move(m).is_err() {
                    break;
                }
                let ply = (i as u32) + 2;
                if in_ply_range(ply, opts.min_ply, opts.max_ply) {
                    collect_sfen(&pos.to_sfen(), opts.mirror_dedup, opts.emit_mirror, &mut sfens);
                }
            }
        }
        Mode::Nth => {
            if !opts.nth.is_empty() {
                if opts.nth.contains(&1) && in_ply_range(1, opts.min_ply, opts.max_ply) {
                    collect_sfen(&pos.to_sfen(), opts.mirror_dedup, opts.emit_mirror, &mut sfens);
                }
                for (i, m) in moves.iter().enumerate() {
                    let ply = (i as u32) + 2;
                    if pos.apply_csa_move(m).is_err() {
                        break;
                    }
                    if opts.nth.contains(&ply) && in_ply_range(ply, opts.min_ply, opts.max_ply) {
                        collect_sfen(
                            &pos.to_sfen(),
                            opts.mirror_dedup,
                            opts.emit_mirror,
                            &mut sfens,
                        );
                    }
                }
            }
        }
    }

    GameResult {
        sfens,
        error: false,
        rating_skipped: false,
        no_rating: false,
    }
}

/// SFEN を収集。mirror_dedup 時は canonical 形式で格納（dedup キーも canonical になる）
fn collect_sfen(sfen: &str, mirror_dedup: bool, emit_mirror: bool, out: &mut Vec<String>) {
    if mirror_dedup {
        let s = canonicalize_4t_with_mirror(sfen).unwrap_or_else(|| sfen.to_string());
        out.push(s);
    } else {
        out.push(sfen.to_string());
        if emit_mirror && let Some(ms) = mirror_horizontal(sfen) {
            out.push(ms);
        }
    }
}

fn run_extract(root: &str, out: &str, opts: &ExtractOptions) -> Result<()> {
    let root = Path::new(root);
    let files = visit_csa_files(root)?;
    let num_files = files.len();
    eprintln!("Found {num_files} CSA files in {root:?}");

    // rayon で並列パース → 各ゲームの SFEN リストを収集
    let results: Vec<GameResult> =
        files.par_iter().map(|p| extract_sfens_from_game(p, opts)).collect();

    // 逐次で dedup + 書き出し（per_game_cap は dedup 後の書き出し数でカウント）
    let mut out_w = open_writer(out)?;
    let mut dedup = DedupSet::new(opts.mirror_dedup);
    let mut wrote = 0usize;
    let mut errors = 0usize;
    let mut rating_skipped = 0usize;
    let mut no_rating = 0usize;
    let mut games_used = 0usize;

    for gr in &results {
        if gr.error {
            errors += 1;
            continue;
        }
        if gr.no_rating {
            no_rating += 1;
            continue;
        }
        if gr.rating_skipped {
            rating_skipped += 1;
            continue;
        }
        if gr.sfens.is_empty() {
            continue;
        }
        games_used += 1;
        let mut written_this_game = 0usize;
        for sfen in &gr.sfens {
            if !opts.mirror_dedup || dedup.insert(sfen) {
                writeln!(out_w, "{sfen}")?;
                wrote += 1;
                written_this_game += 1;
                if opts.per_game_cap > 0 && written_this_game >= opts.per_game_cap {
                    break;
                }
            }
        }
    }

    out_w.close()?;
    eprintln!("Wrote {wrote} SFENs from {games_used} games to {out}");
    if errors > 0 {
        eprintln!("  ({errors} files had errors and were skipped)");
    }
    if opts.min_rating > 0 {
        eprintln!(
            "  ({rating_skipped} games below min_rating={}, {no_rating} games without rating info)",
            opts.min_rating
        );
    }
    if opts.mirror_dedup {
        eprintln!("  (dedup set size: {})", dedup.len());
    }
    Ok(())
}

#[inline]
fn in_ply_range(ply: u32, min_ply: u32, max_ply: u32) -> bool {
    if ply < min_ply {
        return false;
    }
    if max_ply > 0 && ply > max_ply {
        return false;
    }
    true
}
