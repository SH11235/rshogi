use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use reqwest::blocking::Client;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tools::common::csa::parse_csa;
use tools::common::dedup::DedupSet;
use tools::common::floodgate as fg;
use tools::common::io::{open_reader, open_writer, Writer};
use tools::common::sfen_ops::{canonicalize_4t_with_mirror, mirror_horizontal};

#[derive(Parser)]
#[command(
    name = "floodgate-pipeline",
    version,
    about = "Floodgate → CSA → SFEN → mirror → dedup"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download 00LIST.floodgate index
    FetchIndex {
        /// Root URL (HTTP only)
        #[arg(long, default_value = fg::DEFAULT_ROOT)]
        root: String,
        /// Output file path
        #[arg(long, default_value = "00LIST.floodgate")]
        out: String,
    },
    /// Download CSA logs listed by an index file
    Download {
        /// Path to 00LIST.floodgate
        #[arg(long, default_value = "00LIST.floodgate")]
        index: String,
        /// Root URL (HTTP only)
        #[arg(long, default_value = fg::DEFAULT_ROOT)]
        root: String,
        /// Output directory
        #[arg(long, default_value = "logs/x")]
        out_dir: String,
        /// Limit number of files (for testing)
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Extract SFENs from local CSA files
    Extract {
        /// Root directory containing downloaded .csa (mirrors index paths, e.g., logs/x/2025/01/*.csa)
        #[arg(long, default_value = "logs/x")]
        root: String,
        /// Output path ("-" for stdout; supports .gz)
        #[arg(long, default_value = "sfens.txt")]
        out: String,
        /// Extraction mode
        #[arg(long, value_enum, default_value_t = Mode::All)]
        mode: Mode,
        /// When mode=nth, extract at these ply numbers (can repeat)
        #[arg(long, value_delimiter = ',')]
        nth: Vec<u32>,
        /// Canonicalize with horizontal mirror and deduplicate
        #[arg(long)]
        mirror_dedup: bool,
        /// Also emit a horizontally mirrored position per SFEN (use with --mirror-dedup=false)
        #[arg(long)]
        emit_mirror: bool,
        /// Keep only positions with ply >= this (inclusive). 1 means initial position.
        #[arg(long, default_value_t = 1)]
        min_ply: u32,
        /// Keep only positions with ply <= this (inclusive). 0 to disable upper bound.
        #[arg(long, default_value_t = 0)]
        max_ply: u32,
        /// Cap positions written per game (0 = unlimited)
        #[arg(long, default_value_t = 0)]
        per_game_cap: usize,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Mode {
    Initial,
    All,
    Nth,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::FetchIndex { root, out } => run_fetch_index(&root, &out),
        Cmd::Download {
            index,
            root,
            out_dir,
            limit,
        } => run_download(&index, &root, &out_dir, limit),
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
        ),
    }
}

fn run_fetch_index(root: &str, out: &str) -> Result<()> {
    let url = fg::join_url(root, "00LIST.floodgate")?;
    let client = Client::builder().build()?;
    let text = fg::http_get_text(&client, &url)?;
    fs::write(out, text).with_context(|| format!("write index: {out}"))?;
    eprintln!("wrote {out}");
    Ok(())
}

fn run_download(index: &str, root: &str, out_dir: &str, limit: Option<usize>) -> Result<()> {
    let client = Client::builder().build()?;
    let r = open_reader(index)?;
    let lines = fg::parse_index_lines(r)?;
    let count = limit.unwrap_or(lines.len());
    for rel in lines.into_iter().take(count) {
        let url = fg::join_url(root, &rel)?;
        let out_path = fg::local_path_for(Path::new(out_dir), &rel);
        let _ = fg::http_get_to_file_noclobber(&client, &url, &out_path)?;
        eprintln!("saved {}", out_path.display());
    }
    Ok(())
}

fn visit_csa_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let p = entry.path();
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("csa") {
                    files.push(p.to_path_buf());
                }
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
) -> Result<()> {
    let root = Path::new(root);
    let files = visit_csa_files(root)?;
    let mut out_w = open_writer(out)?;
    let mut dedup = DedupSet::new(mirror_dedup);
    let mut wrote = 0usize;
    'games: for p in files {
        let text = fs::read_to_string(&p).with_context(|| format!("read CSA: {}", p.display()))?;
        let (mut pos, moves) = parse_csa(&text)?;
        let mut written_this_game = 0usize;
        match mode {
            Mode::Initial => {
                let sfen = pos.to_sfen();
                if in_ply_range(1, min_ply, max_ply) {
                    let w = maybe_write(&mut out_w, &mut dedup, &sfen, mirror_dedup, emit_mirror)?;
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
                    if per_game_cap > 0 && w > 0 {
                        written_this_game += w;
                    }
                    wrote += w;
                    if per_game_cap > 0 && written_this_game >= per_game_cap {
                        continue 'games;
                    }
                }
                for (i, m) in moves.iter().enumerate() {
                    pos.apply_csa_move(m).ok();
                    let sfen = pos.to_sfen();
                    let ply = (i as u32) + 2;
                    if in_ply_range(ply, min_ply, max_ply) {
                        let w =
                            maybe_write(&mut out_w, &mut dedup, &sfen, mirror_dedup, emit_mirror)?;
                        if per_game_cap > 0 && w > 0 {
                            written_this_game += w;
                        }
                        wrote += w;
                        if per_game_cap > 0 && written_this_game >= per_game_cap {
                            continue 'games;
                        }
                    }
                }
            }
            Mode::Nth => {
                if nth.is_empty() {
                    continue;
                }
                if nth.contains(&1) {
                    let sfen = pos.to_sfen();
                    if in_ply_range(1, min_ply, max_ply) {
                        let w =
                            maybe_write(&mut out_w, &mut dedup, &sfen, mirror_dedup, emit_mirror)?;
                        if per_game_cap > 0 && w > 0 {
                            written_this_game += w;
                        }
                        wrote += w;
                        if per_game_cap > 0 && written_this_game >= per_game_cap {
                            continue 'games;
                        }
                    }
                }
                for (i, m) in moves.iter().enumerate() {
                    let ply = (i as u32) + 2; // after applying i-th (0-based), ply in to_sfen increments starting at 1
                    pos.apply_csa_move(m).ok();
                    if nth.contains(&ply) {
                        let sfen = pos.to_sfen();
                        if in_ply_range(ply, min_ply, max_ply) {
                            let w = maybe_write(
                                &mut out_w,
                                &mut dedup,
                                &sfen,
                                mirror_dedup,
                                emit_mirror,
                            )?;
                            if per_game_cap > 0 && w > 0 {
                                written_this_game += w;
                            }
                            wrote += w;
                            if per_game_cap > 0 && written_this_game >= per_game_cap {
                                continue 'games;
                            }
                        }
                    }
                }
            }
        }
    }
    out_w.close()?;
    eprintln!("wrote {wrote} sfens to {out}");
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
        writeln!(out_w, "{}", s)?;
        written += 1;

        // optionally emit mirror as a separate line when not deduping-by-mirror
        if emit_mirror && !mirror_dedup {
            if let Some(ms) = mirror_horizontal(sfen) {
                writeln!(out_w, "{}", ms)?;
                written += 1;
            }
        }
    }
    Ok(written)
}
