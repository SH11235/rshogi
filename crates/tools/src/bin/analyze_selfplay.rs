/// 自己対局ログの集計ツール
///
/// 使い方:
///   # 明示的なファイルパス指定
///   analyze_selfplay file1.jsonl file2.jsonl
///
///   # glob展開はシェル側で行う
///   analyze_selfplay runs/selfplay/20260206-14*.jsonl
///
///   # JSON出力モード
///   analyze_selfplay --json file1.jsonl file2.jsonl
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(about = "自己対局ログの集計")]
struct Cli {
    /// 集計対象のJSONLファイルパス（複数指定可）
    #[arg(required = true)]
    files: Vec<String>,

    /// JSON出力モード
    #[arg(long)]
    json: bool,
}

// ---------------------------------------------------------------------------
// JSONL読み取り用の構造体（デシリアライズのみ）
// ---------------------------------------------------------------------------

/// 通常JSONLのmeta行
#[derive(Deserialize)]
struct MetaLog {
    settings: MetaSettings,
    engine_cmd: EngineCommandMeta,
}

#[derive(Deserialize)]
struct MetaSettings {
    games: u32,
}

#[derive(Deserialize)]
struct EngineCommandMeta {
    path_black: String,
    path_white: String,
}

/// 通常JSONLのresult行
#[derive(Deserialize)]
struct ResultLog {
    outcome: String,
}

/// summary JSONLの行
#[derive(Deserialize)]
struct SummaryLog {
    total_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    engine_black: EngineSummary,
    engine_white: EngineSummary,
}

#[derive(Deserialize)]
struct EngineSummary {
    path: String,
}

// ---------------------------------------------------------------------------
// 集計用の構造体
// ---------------------------------------------------------------------------

/// 1ファイルのパース結果
struct FileResult {
    black: String,
    white: String,
    games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    done: u32,
}

/// 対戦カード（先手, 後手）ごとの集計
#[derive(Default)]
struct MatchupStats {
    total: u32,
    done: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    files: u32,
}

/// エンジン別の集計
#[derive(Default)]
struct EngineStats {
    games: u32,
    wins: u32,
    losses: u32,
    draws: u32,
}

/// 直接対決の集計（先後合算、正規化済み）
#[derive(Default)]
struct HeadToHeadStats {
    done: u32,
    left_wins: u32,
    right_wins: u32,
    draws: u32,
}

/// JSON出力用
#[derive(Serialize)]
struct JsonOutput {
    files: u32,
    progress: Progress,
    matchups: Vec<JsonMatchup>,
    engines: Vec<JsonEngine>,
    head_to_head: Vec<JsonHeadToHead>,
}

#[derive(Serialize)]
struct Progress {
    done: u32,
    total: u32,
    percent: f64,
}

#[derive(Serialize)]
struct JsonMatchup {
    black: String,
    white: String,
    done: u32,
    total: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    files: u32,
}

#[derive(Serialize)]
struct JsonEngine {
    id: String,
    games: u32,
    wins: u32,
    losses: u32,
    draws: u32,
    win_rate: f64,
}

#[derive(Serialize)]
struct JsonHeadToHead {
    engine_a: String,
    engine_b: String,
    done: u32,
    a_wins: u32,
    b_wins: u32,
    draws: u32,
    a_win_rate: f64,
    elo_diff: Option<f64>,
    elo_ci95: Option<f64>,
}

// ---------------------------------------------------------------------------
// エンジンID抽出
// ---------------------------------------------------------------------------

/// パスから `rshogi-usi-HASH` パターンのハッシュ部分（先頭8文字）を抽出する。
/// 該当しない場合はファイル名全体を返す。
fn extract_engine_id(path: &str) -> String {
    let filename = Path::new(path).file_name().and_then(|s| s.to_str()).unwrap_or(path);

    if let Some(rest) = filename.strip_prefix("rshogi-usi-") {
        // ハッシュ部分の先頭8文字を取る
        let hash: String = rest.chars().take(8).collect();
        if !hash.is_empty() {
            return hash;
        }
    }
    filename.to_string()
}

// ---------------------------------------------------------------------------
// ファイルパース
// ---------------------------------------------------------------------------

fn parse_summary_file(path: &str) -> Result<FileResult> {
    let file =
        std::fs::File::open(path).with_context(|| format!("ファイルを開けません: {path}"))?;
    let reader = BufReader::new(file);

    // summary ファイルは通常1行
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let summary: SummaryLog =
            serde_json::from_str(&line).with_context(|| format!("JSONパースエラー: {path}"))?;
        let done = summary.black_wins + summary.white_wins + summary.draws;
        return Ok(FileResult {
            black: extract_engine_id(&summary.engine_black.path),
            white: extract_engine_id(&summary.engine_white.path),
            games: summary.total_games,
            black_wins: summary.black_wins,
            white_wins: summary.white_wins,
            draws: summary.draws,
            done,
        });
    }
    bail!("空のsummaryファイル: {path}");
}

fn parse_normal_file(path: &str) -> Result<FileResult> {
    let file =
        std::fs::File::open(path).with_context(|| format!("ファイルを開けません: {path}"))?;
    let reader = BufReader::new(file);

    let mut games: u32 = 0;
    let mut black = String::new();
    let mut white = String::new();
    let mut black_wins: u32 = 0;
    let mut white_wins: u32 = 0;
    let mut draws: u32 = 0;
    let mut meta_parsed = false;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 高速フィルタ: type フィールドで判別
        if !meta_parsed && trimmed.contains("\"type\":\"meta\"") {
            let meta: MetaLog = serde_json::from_str(trimmed)
                .with_context(|| format!("metaパースエラー: {path}"))?;
            games = meta.settings.games;
            black = extract_engine_id(&meta.engine_cmd.path_black);
            white = extract_engine_id(&meta.engine_cmd.path_white);
            meta_parsed = true;
        } else if trimmed.contains("\"type\":\"result\"") {
            let result: ResultLog = serde_json::from_str(trimmed)
                .with_context(|| format!("resultパースエラー: {path}"))?;
            match result.outcome.as_str() {
                "black_win" => black_wins += 1,
                "white_win" => white_wins += 1,
                "draw" => draws += 1,
                _ => {}
            }
        }
        // move行・metrics行等はスキップ
    }

    let done = black_wins + white_wins + draws;
    Ok(FileResult {
        black,
        white,
        games,
        black_wins,
        white_wins,
        draws,
        done,
    })
}

fn parse_file(path: &str) -> Result<FileResult> {
    if path.contains(".summary.") {
        parse_summary_file(path)
    } else {
        parse_normal_file(path)
    }
}

// ---------------------------------------------------------------------------
// Elo計算
// ---------------------------------------------------------------------------

/// スコア（勝率）からEloレーティング差を計算する。
/// `score = (wins + draws * 0.5) / total`
/// `Elo = -400 * log10(1/score - 1)`
fn elo_diff(wins: u32, losses: u32, draws: u32) -> Option<f64> {
    let total = wins + losses + draws;
    if total == 0 {
        return None;
    }
    let score = (wins as f64 + draws as f64 * 0.5) / total as f64;
    if score <= 0.0 || score >= 1.0 {
        return None;
    }
    Some(-400.0 * (1.0 / score - 1.0).log10())
}

/// Elo差の95%信頼区間を計算する（正規近似）。
/// 標準誤差: SE = sqrt(score * (1 - score) / n)
/// Elo の SE ≈ dElo/dscore * SE_score
///   dElo/dscore = 400 / (ln(10) * score * (1 - score))
fn elo_ci95(wins: u32, losses: u32, draws: u32) -> Option<f64> {
    let total = wins + losses + draws;
    if total == 0 {
        return None;
    }
    let n = total as f64;
    let score = (wins as f64 + draws as f64 * 0.5) / n;
    if score <= 0.0 || score >= 1.0 {
        return None;
    }
    let se_score = (score * (1.0 - score) / n).sqrt();
    let delo_dscore = 400.0 / (std::f64::consts::LN_10 * score * (1.0 - score));
    let se_elo = (delo_dscore * se_score).abs();
    Some(1.96 * se_elo)
}

// ---------------------------------------------------------------------------
// メイン処理
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 通常の .jsonl が1つでもあれば .summary.jsonl を自動除外（二重カウント防止）
    let has_normal = cli.files.iter().any(|f| !f.contains(".summary."));
    let files: Vec<&str> = cli
        .files
        .iter()
        .filter(|f| {
            if has_normal && f.contains(".summary.") {
                eprintln!("スキップ（summaryは通常ファイルと重複）: {f}");
                false
            } else {
                true
            }
        })
        .map(|s| s.as_str())
        .collect();

    // 全ファイルをパースして集計
    let mut matchups: BTreeMap<(String, String), MatchupStats> = BTreeMap::new();
    let mut engine_ids: BTreeSet<String> = BTreeSet::new();
    let mut valid_files = 0u32;

    for path in &files {
        match parse_file(path) {
            Ok(result) => {
                if result.black.is_empty() || result.white.is_empty() || result.games == 0 {
                    eprintln!("警告: 有効なデータなし: {path}");
                    continue;
                }
                let key = (result.black.clone(), result.white.clone());
                let stats = matchups.entry(key).or_default();
                stats.total += result.games;
                stats.done += result.done;
                stats.black_wins += result.black_wins;
                stats.white_wins += result.white_wins;
                stats.draws += result.draws;
                stats.files += 1;
                engine_ids.insert(result.black);
                engine_ids.insert(result.white);
                valid_files += 1;
            }
            Err(e) => {
                eprintln!("警告: {path}: {e}");
            }
        }
    }

    if matchups.is_empty() {
        bail!("有効な対局データがありません");
    }

    // エンジン名ラベル（A, B, C, ...）を短いハッシュ順に自動割当
    let labels: BTreeMap<String, String> = engine_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let label = format!("{}({})", (b'A' + i as u8) as char, id);
            (id.clone(), label)
        })
        .collect();

    let total_done: u32 = matchups.values().map(|v| v.done).sum();
    let total_all: u32 = matchups.values().map(|v| v.total).sum();

    // エンジン別集計
    let mut engines: BTreeMap<String, EngineStats> = BTreeMap::new();
    for ((b, w), v) in &matchups {
        let be = engines.entry(b.clone()).or_default();
        be.wins += v.black_wins;
        be.losses += v.white_wins;
        be.draws += v.draws;
        be.games += v.done;

        let we = engines.entry(w.clone()).or_default();
        we.wins += v.white_wins;
        we.losses += v.black_wins;
        we.draws += v.draws;
        we.games += v.done;
    }

    // 直接対決集計（先後合算、正規化キー: 辞書順で小さい方がleft）
    let mut head_to_head: BTreeMap<(String, String), HeadToHeadStats> = BTreeMap::new();
    for ((b, w), v) in &matchups {
        let (left, right) = if b <= w {
            (b.clone(), w.clone())
        } else {
            (w.clone(), b.clone())
        };
        let h = head_to_head.entry((left, right)).or_default();
        h.done += v.done;
        h.draws += v.draws;
        if b <= w {
            h.left_wins += v.black_wins;
            h.right_wins += v.white_wins;
        } else {
            h.right_wins += v.black_wins;
            h.left_wins += v.white_wins;
        }
    }

    if cli.json {
        print_json(
            valid_files,
            total_done,
            total_all,
            &matchups,
            &engines,
            &head_to_head,
            &labels,
        )?;
    } else {
        print_text(valid_files, total_done, total_all, &matchups, &engines, &head_to_head, &labels);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// テキスト出力
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn print_text(
    file_count: u32,
    total_done: u32,
    total_all: u32,
    matchups: &BTreeMap<(String, String), MatchupStats>,
    engines: &BTreeMap<String, EngineStats>,
    head_to_head: &BTreeMap<(String, String), HeadToHeadStats>,
    labels: &BTreeMap<String, String>,
) {
    let pct = if total_all > 0 {
        total_done as f64 / total_all as f64 * 100.0
    } else {
        0.0
    };
    println!(
        "ファイル数: {}  進捗: {}/{}局完了 ({:.1}%)",
        file_count, total_done, total_all, pct
    );
    println!();

    // 対戦カード別
    println!("対戦カード別 合算（先手 vs 後手）");
    println!("{}", "=".repeat(75));
    for ((b, w), v) in matchups {
        let bn = labels.get(b).map_or(b.as_str(), |s| s.as_str());
        let wn = labels.get(w).map_or(w.as_str(), |s| s.as_str());
        println!(
            "  {:16}(先手) vs {:16}(後手) | {:3}/{:3}局 | 先手勝:{:3} 後手勝:{:3} 引分:{}",
            bn, wn, v.done, v.total, v.black_wins, v.white_wins, v.draws
        );
    }

    // エンジン別（勝率降順でソート）
    println!();
    println!("エンジン別 勝敗（先後合算）");
    println!("{}", "=".repeat(75));
    let mut engine_list: Vec<_> = engines.iter().collect();
    engine_list.sort_by(|(_, a), (_, b)| {
        let rate_a = win_rate(a.wins, a.losses, a.draws);
        let rate_b = win_rate(b.wins, b.losses, b.draws);
        rate_b.partial_cmp(&rate_a).unwrap_or(std::cmp::Ordering::Equal)
    });
    for (id, s) in &engine_list {
        let name = labels.get(*id).map_or(id.as_str(), |s| s.as_str());
        let wr = win_rate(s.wins, s.losses, s.draws);
        println!(
            "  {:16} | {:3}局完了 | 勝:{:3} 負:{:3} 引分:{:2} | 勝率:{:.1}%",
            name, s.games, s.wins, s.losses, s.draws, wr
        );
    }

    // 直接対決
    println!();
    println!("直接対決（先後合算）");
    println!("{}", "=".repeat(75));
    for ((a, b), v) in head_to_head {
        let an = labels.get(a).map_or(a.as_str(), |s| s.as_str());
        let bn = labels.get(b).map_or(b.as_str(), |s| s.as_str());
        let total = v.left_wins + v.right_wins + v.draws;
        let wr_a = if total > 0 {
            v.left_wins as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        let elo = elo_diff(v.left_wins, v.right_wins, v.draws);
        let ci = elo_ci95(v.left_wins, v.right_wins, v.draws);

        let elo_str = match (elo, ci) {
            (Some(e), Some(c)) => format!(" | Elo差:{:+.0} ±{:.0}", e, c),
            _ => String::new(),
        };

        println!(
            "  {:16} vs {:16} | {:3}局 | {}:{:3}勝 {}:{:3}勝 引分:{} | {}勝率:{:.1}%{}",
            an, bn, v.done, an, v.left_wins, bn, v.right_wins, v.draws, an, wr_a, elo_str
        );
    }
}

fn win_rate(wins: u32, losses: u32, draws: u32) -> f64 {
    let total = wins + losses + draws;
    if total == 0 {
        return 0.0;
    }
    wins as f64 / total as f64 * 100.0
}

// ---------------------------------------------------------------------------
// JSON出力
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn print_json(
    file_count: u32,
    total_done: u32,
    total_all: u32,
    matchups: &BTreeMap<(String, String), MatchupStats>,
    engines: &BTreeMap<String, EngineStats>,
    head_to_head: &BTreeMap<(String, String), HeadToHeadStats>,
    labels: &BTreeMap<String, String>,
) -> Result<()> {
    let pct = if total_all > 0 {
        total_done as f64 / total_all as f64 * 100.0
    } else {
        0.0
    };

    let json_matchups: Vec<JsonMatchup> = matchups
        .iter()
        .map(|((b, w), v)| JsonMatchup {
            black: labels.get(b).cloned().unwrap_or_else(|| b.clone()),
            white: labels.get(w).cloned().unwrap_or_else(|| w.clone()),
            done: v.done,
            total: v.total,
            black_wins: v.black_wins,
            white_wins: v.white_wins,
            draws: v.draws,
            files: v.files,
        })
        .collect();

    let mut engine_list: Vec<_> = engines.iter().collect();
    engine_list.sort_by(|(_, a), (_, b)| {
        let rate_a = win_rate(a.wins, a.losses, a.draws);
        let rate_b = win_rate(b.wins, b.losses, b.draws);
        rate_b.partial_cmp(&rate_a).unwrap_or(std::cmp::Ordering::Equal)
    });
    let json_engines: Vec<JsonEngine> = engine_list
        .iter()
        .map(|(id, s)| JsonEngine {
            id: labels.get(*id).cloned().unwrap_or_else(|| (*id).clone()),
            games: s.games,
            wins: s.wins,
            losses: s.losses,
            draws: s.draws,
            win_rate: win_rate(s.wins, s.losses, s.draws),
        })
        .collect();

    let json_h2h: Vec<JsonHeadToHead> = head_to_head
        .iter()
        .map(|((a, b), v)| JsonHeadToHead {
            engine_a: labels.get(a).cloned().unwrap_or_else(|| a.clone()),
            engine_b: labels.get(b).cloned().unwrap_or_else(|| b.clone()),
            done: v.done,
            a_wins: v.left_wins,
            b_wins: v.right_wins,
            draws: v.draws,
            a_win_rate: {
                let total = v.left_wins + v.right_wins + v.draws;
                if total > 0 {
                    v.left_wins as f64 / total as f64 * 100.0
                } else {
                    0.0
                }
            },
            elo_diff: elo_diff(v.left_wins, v.right_wins, v.draws),
            elo_ci95: elo_ci95(v.left_wins, v.right_wins, v.draws),
        })
        .collect();

    let output = JsonOutput {
        files: file_count,
        progress: Progress {
            done: total_done,
            total: total_all,
            percent: pct,
        },
        matchups: json_matchups,
        engines: json_engines,
        head_to_head: json_h2h,
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
