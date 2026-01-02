//! Floodgate棋譜取得・変換パイプライン
//!
//! # 使用例
//!
//! ```bash
//! # 1. インデックスファイルをダウンロード
//! cargo run -p tools --bin floodgate_pipeline -- fetch-index --out 00LIST.floodgate
//!
//! # 2. CSAファイルをダウンロード
//! cargo run -p tools --bin floodgate_pipeline -- download --index 00LIST.floodgate --out-dir logs/x --limit 100
//!
//! # 3. SFENを抽出
//! cargo run -p tools --bin floodgate_pipeline -- extract --root logs/x --out sfens.txt --mirror-dedup
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
use tools::common::io::{open_writer, Writer};
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
    eprintln!("Fetching index from: {url}");
    let client = Client::builder().build()?;
    let text = fg::http_get_text(&client, &url)?;
    fs::write(out, text).with_context(|| format!("write index: {out}"))?;
    eprintln!("Wrote index to: {out}");
    Ok(())
}

fn run_download(index: &str, root: &str, out_dir: &str, limit: Option<usize>) -> Result<()> {
    let client = Client::builder().build()?;
    let r = tools::common::io::open_reader(index)?;
    let lines = fg::parse_index_lines(r)?;
    let count = limit.unwrap_or(lines.len());
    eprintln!("Downloading {} CSA files (total in index: {})", count, lines.len());
    for (i, rel) in lines.into_iter().take(count).enumerate() {
        let url = fg::join_url(root, &rel)?;
        let out_path = fg::local_path_for(Path::new(out_dir), &rel);
        match fg::http_get_to_file_noclobber(&client, &url, &out_path) {
            Ok(_) => {
                if (i + 1) % 100 == 0 {
                    eprintln!("  Downloaded {}/{} files...", i + 1, count);
                }
            }
            Err(e) => {
                eprintln!("  Warning: failed to download {rel}: {e}");
            }
        }
    }
    eprintln!("Download complete. Files saved to: {out_dir}");
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
    eprintln!("Found {} CSA files in {:?}", files.len(), root);
    let mut out_w = open_writer(out)?;
    let mut dedup = DedupSet::new(mirror_dedup);
    let mut wrote = 0usize;
    let mut errors = 0usize;
    'games: for p in &files {
        let text = match fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                errors += 1;
                log::warn!("Failed to read {}: {e}", p.display());
                continue;
            }
        };
        let (mut pos, moves) = match parse_csa(&text) {
            Ok(r) => r,
            Err(e) => {
                errors += 1;
                log::warn!("Failed to parse {}: {e}", p.display());
                continue;
            }
        };
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
    eprintln!("Wrote {wrote} SFENs to {out}");
    if errors > 0 {
        eprintln!("  ({errors} files had errors and were skipped)");
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
        if emit_mirror && !mirror_dedup {
            if let Some(ms) = mirror_horizontal(sfen) {
                writeln!(out_w, "{ms}")?;
                written += 1;
            }
        }
    }
    Ok(written)
}
