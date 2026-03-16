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
//! # 2. CSAファイルをダウンロード（年 + プレイヤーでフィルタ）
//! cargo run -p tools --bin floodgate_pipeline -- download --year-from 2021 --player-file high_rated.txt
//!
//! # 3. SFENを抽出（レーティングで精密フィルタ）
//! cargo run -p tools --bin floodgate_pipeline -- extract --min-rating 3900 --max-ply 32
//! ```

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use reqwest::blocking::Client;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tools::common::csa::parse_csa;
use tools::common::dedup::DedupSet;
use tools::common::floodgate as fg;
use tools::common::io::{Writer, open_writer};
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
        /// この年以降のファイルのみダウンロード（例: 2021）
        #[arg(long)]
        year_from: Option<u16>,
        /// この年以前のファイルのみダウンロード
        #[arg(long)]
        year_to: Option<u16>,
        /// 高レートプレイヤー名ファイル（1行1名）。両対局者がリストに含まれるゲームのみDL
        #[arg(long)]
        player_file: Option<String>,
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
        /// 1棋譜あたりの最大抽出数（0=無制限）
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
            year_from,
            year_to,
            player_file,
        } => {
            run_download(&index, &root, &out_dir, limit, year_from, year_to, player_file.as_deref())
        }
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
        } => run_extract(
            &root,
            &out,
            mode,
            &nth,
            mirror_dedup,
            emit_mirror,
            min_ply,
            max_ply,
            per_game_cap,
            min_rating,
        ),
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

fn year_of_path(rel: &str) -> Option<u16> {
    rel.get(..4)?.parse::<u16>().ok()
}

fn run_download(
    index: &str,
    root: &str,
    out_dir: &str,
    limit: Option<usize>,
    year_from: Option<u16>,
    year_to: Option<u16>,
    player_file: Option<&str>,
) -> Result<()> {
    let client = Client::builder().build()?;
    let r = tools::common::io::open_reader(index)?;
    let all_lines = fg::parse_index_lines(r)?;
    let total = all_lines.len();

    // プレイヤーフィルタセットの読み込み
    let player_set = if let Some(pf) = player_file {
        let set = fg::load_player_set(Path::new(pf))?;
        eprintln!("Loaded {} player names from {pf}", set.len());
        Some(set)
    } else {
        None
    };

    // 年 + プレイヤーフィルタ
    let lines: Vec<String> = all_lines
        .into_iter()
        .filter(|rel| {
            let year = year_of_path(rel).unwrap_or(0);
            if year_from.is_some_and(|yf| year < yf) || year_to.is_some_and(|yt| year > yt) {
                return false;
            }
            if let Some(ref set) = player_set {
                if let Some((a, b)) = fg::players_from_path(rel) {
                    set.contains(a) && set.contains(b)
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
        "Downloading {} CSA files (total in index: {}, after filter: {})",
        count, total, after_filter
    );
    let mut downloaded = 0usize;
    let mut skipped = 0usize;
    for (i, rel) in lines.into_iter().take(count).enumerate() {
        let url = fg::join_url(root, &rel)?;
        let out_path = fg::local_path_for(Path::new(out_dir), &rel);
        if out_path.exists() {
            skipped += 1;
            continue;
        }
        match fg::http_get_to_file_noclobber(&client, &url, &out_path) {
            Ok(_) => {
                downloaded += 1;
                if downloaded.is_multiple_of(500) {
                    eprintln!(
                        "  Downloaded {downloaded} new files ({}/{count} processed)...",
                        i + 1
                    );
                }
            }
            Err(e) => {
                eprintln!("  Warning: failed to download {rel}: {e}");
            }
        }
    }
    eprintln!("Download complete. {downloaded} new, {skipped} already existed. Dir: {out_dir}");
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

#[allow(clippy::too_many_arguments)]
fn run_extract(
    root: &str,
    out: &str,
    mode: Mode,
    nth: &[u32],
    mirror_dedup: bool,
    emit_mirror: bool,
    min_ply: u32,
    max_ply: u32,
    per_game_cap: usize,
    min_rating: u32,
) -> Result<()> {
    let root = Path::new(root);
    let files = visit_csa_files(root)?;
    eprintln!("Found {} CSA files in {:?}", files.len(), root);
    let mut out_w = open_writer(out)?;
    let mut dedup = DedupSet::new(mirror_dedup);
    let mut wrote = 0usize;
    let mut errors = 0usize;
    let mut rating_skipped = 0usize;
    let mut no_rating = 0usize;
    let mut games_used = 0usize;
    'games: for p in &files {
        let text = match fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                errors += 1;
                log::warn!("Failed to read {}: {e}", p.display());
                continue;
            }
        };
        let (mut pos, moves, info) = match parse_csa(&text) {
            Ok(r) => r,
            Err(e) => {
                errors += 1;
                log::warn!("Failed to parse {}: {e}", p.display());
                continue;
            }
        };
        // レーティングフィルタ
        if min_rating > 0 {
            if info.black_rating.is_none() || info.white_rating.is_none() {
                no_rating += 1;
                continue 'games;
            }
            if !info.both_ratings_at_least(min_rating as f64) {
                rating_skipped += 1;
                continue 'games;
            }
        }
        games_used += 1;
        let mut written_this_game = 0usize;
        match mode {
            Mode::Initial => {
                let sfen = pos.to_sfen();
                if in_ply_range(1, min_ply, max_ply) {
                    let w = maybe_write(&mut out_w, &mut dedup, &sfen, mirror_dedup, emit_mirror)?;
                    wrote += w;
                    if per_game_cap > 0 && w > 0 {
                        written_this_game += w;
                        if written_this_game >= per_game_cap {
                            continue 'games;
                        }
                    }
                }
            }
            Mode::All => {
                // include initial position if range covers ply 1
                if in_ply_range(1, min_ply, max_ply) {
                    let sfen = pos.to_sfen();
                    let w = maybe_write(&mut out_w, &mut dedup, &sfen, mirror_dedup, emit_mirror)?;
                    wrote += w;
                    if per_game_cap > 0 && w > 0 {
                        written_this_game += w;
                        if written_this_game >= per_game_cap {
                            continue 'games;
                        }
                    }
                }
                for (i, m) in moves.iter().enumerate() {
                    if pos.apply_csa_move(m).is_err() {
                        break;
                    }
                    let sfen = pos.to_sfen();
                    let ply = (i as u32) + 2;
                    if in_ply_range(ply, min_ply, max_ply) {
                        let w =
                            maybe_write(&mut out_w, &mut dedup, &sfen, mirror_dedup, emit_mirror)?;
                        wrote += w;
                        if per_game_cap > 0 && w > 0 {
                            written_this_game += w;
                            if written_this_game >= per_game_cap {
                                continue 'games;
                            }
                        }
                    }
                }
            }
            Mode::Nth => {
                if nth.is_empty() {
                    continue;
                }
                if nth.contains(&1) && in_ply_range(1, min_ply, max_ply) {
                    let sfen = pos.to_sfen();
                    let w = maybe_write(&mut out_w, &mut dedup, &sfen, mirror_dedup, emit_mirror)?;
                    wrote += w;
                    if per_game_cap > 0 && w > 0 {
                        written_this_game += w;
                        if written_this_game >= per_game_cap {
                            continue 'games;
                        }
                    }
                }
                for (i, m) in moves.iter().enumerate() {
                    let ply = (i as u32) + 2;
                    if pos.apply_csa_move(m).is_err() {
                        break;
                    }
                    if nth.contains(&ply) && in_ply_range(ply, min_ply, max_ply) {
                        let sfen = pos.to_sfen();
                        let w =
                            maybe_write(&mut out_w, &mut dedup, &sfen, mirror_dedup, emit_mirror)?;
                        wrote += w;
                        if per_game_cap > 0 && w > 0 {
                            written_this_game += w;
                            if written_this_game >= per_game_cap {
                                continue 'games;
                            }
                        }
                    }
                }
            }
        }
    }
    out_w.close()?;
    eprintln!("Wrote {wrote} SFENs from {games_used} games to {out}");
    if errors > 0 {
        eprintln!("  ({errors} files had errors and were skipped)");
    }
    if min_rating > 0 {
        eprintln!(
            "  ({rating_skipped} games below min_rating={min_rating}, {no_rating} games without rating info)"
        );
    }
    if mirror_dedup {
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

fn maybe_write(
    out_w: &mut Writer,
    dedup: &mut DedupSet,
    sfen: &str,
    mirror_dedup: bool,
    emit_mirror: bool,
) -> Result<usize> {
    let mut written = 0usize;
    if !mirror_dedup || dedup.insert(sfen) {
        // write original (or canonicalized when mirror_dedup)
        let s = if mirror_dedup {
            canonicalize_4t_with_mirror(sfen).unwrap_or_else(|| sfen.to_string())
        } else {
            sfen.to_string()
        };
        writeln!(out_w, "{s}")?;
        written += 1;

        // optionally emit mirror as a separate line when not deduping-by-mirror
        if emit_mirror
            && !mirror_dedup
            && let Some(ms) = mirror_horizontal(sfen)
        {
            writeln!(out_w, "{ms}")?;
            written += 1;
        }
    }
    Ok(written)
}
