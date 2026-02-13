/// round-robin 並列トーナメント。
///
/// crossbeam-channel ワーカーモデルで複数エンジン間の総当たり対局を並列実行する。
/// 出力は analyze_selfplay 互換の JSONL 形式。
///
/// # 使用例
///
/// 同一バイナリで異なる評価関数を比較（--engine-label 必須）:
/// ```shell
/// cargo run -p tools --release --bin tournament -- \
///   --engine target/release/rshogi-usi --engine-label nnue-v60 \
///   --engine target/release/rshogi-usi --engine-label material \
///   --games 50 --byoyomi 500 --threads 2 \
///   --engine-usi-option "0:EvalFile=eval/halfka_hm_512x2-8-64_crelu/v60.bin" \
///   --out-dir "runs/selfplay/$(date +%Y%m%d_%H%M%S)-nnue-v60-vs-material9"
/// ```
///
/// rshogi vs YaneuraOu（suisho5, HalfKP 256x2-32-32, FV_SCALE=24）:
/// ```shell
/// cargo build --release -p rshogi-usi && \
/// cargo run -p tools --release --bin tournament -- \
///   --concurrency 8 \
///   --engine target/release/rshogi-usi --engine-label rshogi \
///   --engine /mnt/nvme1/development/YaneuraOu/source/YaneuraOu-halfkp_256x2-32-32 --engine-label yaneuraou \
///   --games 50 --byoyomi 500 --threads 2 \
///   --usi-option "FV_SCALE=24" \
///   --engine-usi-option "0:EvalFile=eval/halfkp_256x2-32-32_crelu/suisho5.bin" \
///   --engine-usi-option "1:EvalDir=/mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu" \
///   --engine-usi-option "1:BookFile=no_book" \
///   --engine-usi-option "1:MinimumThinkingTime=0" \
///   --engine-usi-option "1:NetworkDelay=0" \
///   --engine-usi-option "1:NetworkDelay2=0" \
///   --engine-usi-option "1:RoundUpToFullSecond=false" \
///   --out-dir "runs/selfplay/$(date +%Y%m%d_%H%M%S)-rshogi-vs-yaneuraou-suisho5"
/// ```
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::Local;
use clap::Parser as _;
use crossbeam_channel as chan;
use rand::prelude::IndexedRandom;
use serde::Serialize;

use tools::selfplay::game::{run_game, GameConfig, MoveEvent};
use tools::selfplay::time_control::TimeControl;
use tools::selfplay::types::{side_label, EvalLog};
use tools::selfplay::{
    load_start_positions, EngineConfig, EngineProcess, GameOutcome, ParsedPosition,
};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(clap::Parser, Debug)]
#[command(about = "round-robin parallel tournament for rshogi-usi engines")]
struct Cli {
    /// Engine binary paths (2 or more required)
    #[arg(long = "engine", required = true, num_args = 1)]
    engines: Vec<PathBuf>,

    /// Engine labels (must match --engine count if specified).
    /// Required when the same binary path appears more than once.
    #[arg(long = "engine-label", num_args = 1)]
    engine_labels: Vec<String>,

    /// Number of games per direction for each pair
    #[arg(long, default_value_t = 100)]
    games: u32,

    /// Number of concurrent workers
    #[arg(long, default_value_t = 1)]
    concurrency: usize,

    /// Byoyomi time per move in milliseconds
    #[arg(long, default_value_t = 2000)]
    byoyomi: u64,

    /// Threads per engine
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Hash/USI_Hash size (MiB) per engine
    #[arg(long, default_value_t = 256)]
    hash_mb: u32,

    /// Additional USI options (format: "Name=Value", can be repeated)
    #[arg(long = "usi-option", num_args = 1..)]
    usi_options: Option<Vec<String>>,

    /// Per-engine USI options (format: "INDEX:Name=Value", can be repeated).
    /// Overrides --usi-option for that engine (not merged).
    #[arg(long = "engine-usi-option", num_args = 1..)]
    engine_usi_options: Option<Vec<String>>,

    /// Maximum plies per game
    #[arg(long, default_value_t = 512)]
    max_moves: u32,

    /// Output directory (required)
    #[arg(long)]
    out_dir: PathBuf,

    /// Start position file (USI position lines, one per line)
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// Report progress every N games
    #[arg(long, default_value_t = 10)]
    report_interval: u32,

    /// Safety margin for timeout detection (ms)
    #[arg(long, default_value_t = 1000)]
    timeout_margin_ms: u64,
}

// ---------------------------------------------------------------------------
// チケットと結果
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MatchTicket {
    /// グローバル一意 ID
    id: u64,
    /// engines[black_idx] が先手
    black_idx: usize,
    /// engines[white_idx] が後手
    white_idx: usize,
    /// 開始局面インデックス
    startpos_idx: usize,
}

struct MatchResult {
    ticket: MatchTicket,
    outcome: GameOutcome,
    reason: String,
    plies: u32,
    move_logs: Vec<MoveLogEntry>,
}

#[derive(Clone, Serialize)]
struct MoveLogEntry {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    ply: u32,
    side_to_move: char,
    sfen_before: String,
    move_usi: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_move_usi: Option<String>,
    engine: String,
    elapsed_ms: u64,
    think_limit_ms: u64,
    timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    eval: Option<EvalLog>,
}

#[derive(Serialize)]
struct ResultLogEntry<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    outcome: &'a str,
    reason: &'a str,
    plies: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    winner: Option<String>,
}

#[derive(Serialize)]
struct MetaLogEntry {
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
    settings: MetaSettings,
    engine_cmd: EngineCommandMeta,
    start_positions: Vec<String>,
    output: String,
}

#[derive(Serialize)]
struct MetaSettings {
    games: u32,
    max_moves: u32,
    byoyomi: u64,
    timeout_margin_ms: u64,
    threads: usize,
    hash_mb: u32,
}

#[derive(Serialize)]
struct EngineCommandMeta {
    path_black: String,
    path_white: String,
    label_black: String,
    label_white: String,
}

// ---------------------------------------------------------------------------
// ペア別ライター
// ---------------------------------------------------------------------------

/// (black_idx, white_idx) → ファイルへの BufWriter
struct PairWriter {
    writer: BufWriter<File>,
}

impl PairWriter {
    fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let file =
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    fn write_json(&mut self, value: &impl Serialize) -> Result<()> {
        serde_json::to_writer(&mut self.writer, value)?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ワーカースレッド
// ---------------------------------------------------------------------------

struct WorkerConfig {
    engine_paths: Vec<PathBuf>,
    engine_labels: Vec<String>,
    engine_usi_options: Vec<Vec<String>>,
    threads: usize,
    hash_mb: u32,
    max_moves: u32,
    timeout_margin_ms: u64,
    byoyomi: u64,
    start_positions: Vec<ParsedPosition>,
}

fn worker_main(
    cfg: WorkerConfig,
    rx: chan::Receiver<Option<MatchTicket>>,
    tx: chan::Sender<MatchResult>,
    shutdown: Arc<AtomicBool>,
) {
    let WorkerConfig {
        engine_paths,
        engine_labels,
        engine_usi_options,
        threads,
        hash_mb,
        max_moves,
        timeout_margin_ms,
        byoyomi,
        start_positions,
    } = cfg;
    // ワーカー内で全エンジンを起動
    let mut engines: Vec<EngineProcess> = Vec::new();
    for (i, path) in engine_paths.iter().enumerate() {
        let label = engine_labels[i].clone();
        let cfg = EngineConfig {
            path: path.clone(),
            args: Vec::new(),
            threads,
            hash_mb,
            network_delay: None,
            network_delay2: None,
            minimum_thinking_time: None,
            slowmover: None,
            ponder: false,
            usi_options: engine_usi_options[i].clone(),
        };
        match EngineProcess::spawn(&cfg, label) {
            Ok(ep) => engines.push(ep),
            Err(e) => {
                eprintln!("worker: failed to spawn engine {i} ({}): {e}", path.display());
                shutdown.store(true, Ordering::Relaxed);
                return;
            }
        }
    }

    while let Ok(Some(ticket)) = rx.recv() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // 2つの異なるインデックスへの同時可変借用のため split_at_mut を使用
        let (black, white) = if ticket.black_idx < ticket.white_idx {
            let (left, right) = engines.split_at_mut(ticket.white_idx);
            (&mut left[ticket.black_idx], &mut right[0])
        } else {
            let (left, right) = engines.split_at_mut(ticket.black_idx);
            (&mut right[0], &mut left[ticket.white_idx])
        };

        let _ = black.new_game();
        let _ = white.new_game();

        let start_pos = &start_positions[ticket.startpos_idx];
        let tc = TimeControl::new(0, 0, 0, 0, byoyomi);
        let config = GameConfig {
            max_moves,
            timeout_margin_ms,
            pass_rights: None,
        };
        let game_id = (ticket.id as u32) + 1;

        let mut move_logs: Vec<MoveLogEntry> = Vec::new();
        let mut on_move = |event: &MoveEvent| {
            move_logs.push(MoveLogEntry {
                kind: "move",
                game_id,
                ply: event.ply,
                side_to_move: side_label(event.side),
                sfen_before: event.sfen_before.clone(),
                move_usi: event.move_usi.clone(),
                raw_move_usi: event.raw_move_usi.clone(),
                engine: event.engine_label.clone(),
                elapsed_ms: event.elapsed_ms,
                think_limit_ms: event.think_limit_ms,
                timed_out: event.timed_out,
                eval: event.eval.clone(),
            });
        };

        match run_game(black, white, start_pos, tc, &config, game_id, &mut on_move, None) {
            Ok(result) => {
                let _ = tx.send(MatchResult {
                    ticket,
                    outcome: result.outcome,
                    reason: result.reason,
                    plies: result.plies,
                    move_logs,
                });
            }
            Err(e) => {
                eprintln!("worker: game error: {e}");
                let _ = tx.send(MatchResult {
                    ticket,
                    outcome: GameOutcome::Draw,
                    reason: format!("error: {e}"),
                    plies: 0,
                    move_logs,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// メイン
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.engines.len() < 2 {
        bail!("at least 2 engines are required");
    }
    if cli.concurrency == 0 {
        bail!("--concurrency must be at least 1");
    }

    // バイナリ存在確認
    for path in &cli.engines {
        if !path.is_file() {
            bail!("engine binary not found: {}", path.display());
        }
    }

    let n = cli.engines.len();

    // エンジンラベルの解決
    let engine_labels: Vec<String> = if cli.engine_labels.is_empty() {
        // ラベル未指定: 同一パスが重複していないか確認
        let mut seen: HashMap<&Path, usize> = HashMap::new();
        for (i, p) in cli.engines.iter().enumerate() {
            if let Some(prev) = seen.insert(p.as_path(), i) {
                bail!(
                    "同一バイナリが複数指定されています (engines[{prev}] と engines[{i}]: {})。\n\
                     --engine-label で各エンジンにラベルを付けてください。",
                    p.display()
                );
            }
        }
        cli.engines.iter().map(|p| engine_label_from_path(p)).collect()
    } else {
        if cli.engine_labels.len() != n {
            bail!(
                "--engine-label の数 ({}) が --engine の数 ({n}) と一致しません",
                cli.engine_labels.len()
            );
        }
        cli.engine_labels.clone()
    };

    // ラベルの重複チェック
    {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for (i, label) in engine_labels.iter().enumerate() {
            if let Some(prev) = seen.insert(label.as_str(), i) {
                bail!(
                    "ラベル '{}' が重複しています (engines[{prev}] と engines[{i}])。\n\
                     各エンジンには一意のラベルを指定してください。",
                    label
                );
            }
        }
    }

    // 開始局面のロード
    let (start_defs, start_commands) =
        load_start_positions(cli.startpos_file.as_deref(), None, None, None)?;

    // 出力ディレクトリの作成
    fs::create_dir_all(&cli.out_dir)
        .with_context(|| format!("failed to create {}", cli.out_dir.display()))?;

    let common_usi_options = cli.usi_options.clone().unwrap_or_default();

    // per-engine オプションを解析: HashMap<usize, Vec<String>>
    let mut per_engine_usi: HashMap<usize, Vec<String>> = HashMap::new();
    if let Some(opts) = &cli.engine_usi_options {
        for opt in opts {
            let (idx_str, kv) = opt
                .split_once(':')
                .with_context(|| format!("invalid --engine-usi-option format: {opt}"))?;
            let idx: usize =
                idx_str.parse().with_context(|| format!("invalid engine index: {idx_str}"))?;
            if idx >= n {
                bail!("--engine-usi-option index {idx} out of range (0..{n})");
            }
            per_engine_usi.entry(idx).or_default().push(kv.to_string());
        }
    }

    // エンジンごとの最終オプションリストを構築
    let engine_usi_options: Vec<Vec<String>> = (0..n)
        .map(|i| per_engine_usi.remove(&i).unwrap_or_else(|| common_usi_options.clone()))
        .collect();
    let timestamp = Local::now();
    let shutdown = Arc::new(AtomicBool::new(false));

    // Ctrl-C ハンドラ
    {
        let shutdown_clone = shutdown.clone();
        ctrlc::set_handler(move || {
            eprintln!("\nShutting down gracefully...");
            shutdown_clone.store(true, Ordering::Relaxed);
        })
        .ok();
    }

    // 全ペア × games × 2方向のチケット生成
    // cli.games は「各方向の対局数」なので、1ペアあたり cli.games * 2 局
    let total_per_pair = cli.games * 2;
    let mut tickets: Vec<MatchTicket> = Vec::new();
    let mut rng = rand::rng();
    {
        let mut ticket_id = 0u64;
        for i in 0..n {
            for j in (i + 1)..n {
                for game_idx in 0..total_per_pair {
                    // game_idx が偶数のとき (i, j), 奇数のとき (j, i) で先後交替
                    let (black_idx, white_idx) = if game_idx % 2 == 0 { (i, j) } else { (j, i) };
                    let startpos_idx = if start_defs.len() == 1 {
                        0
                    } else {
                        // ランダムに選択
                        *((0..start_defs.len()).collect::<Vec<_>>()).choose(&mut rng).unwrap()
                    };
                    tickets.push(MatchTicket {
                        id: ticket_id,
                        black_idx,
                        white_idx,
                        startpos_idx,
                    });
                    ticket_id += 1;
                }
            }
        }
    }

    let total_games = tickets.len() as u32;
    println!(
        "tournament: {} engines, {} pairs, {} games/direction, {} games/pair, {} total games, concurrency={}",
        n,
        n * (n - 1) / 2,
        cli.games,
        total_per_pair,
        total_games,
        cli.concurrency
    );

    // ペア別のファイルライターを準備し、meta行を書き出す
    let mut pair_writers: HashMap<(usize, usize), PairWriter> = HashMap::new();
    // ペアごとのゲームカウンター
    let mut pair_game_count: HashMap<(usize, usize), u32> = HashMap::new();

    for i in 0..n {
        for j in (i + 1)..n {
            let filename = format!("{}-vs-{}.jsonl", engine_labels[i], engine_labels[j]);
            let path = cli.out_dir.join(&filename);
            let mut pw = PairWriter::new(&path)?;

            // meta行を書く
            let meta = MetaLogEntry {
                kind: "meta".to_string(),
                timestamp: timestamp.to_rfc3339(),
                settings: MetaSettings {
                    games: cli.games * 2, // 各方向 cli.games 局、双方向で合計
                    max_moves: cli.max_moves,
                    byoyomi: cli.byoyomi,
                    timeout_margin_ms: cli.timeout_margin_ms,
                    threads: cli.threads,
                    hash_mb: cli.hash_mb,
                },
                engine_cmd: EngineCommandMeta {
                    path_black: cli.engines[i].display().to_string(),
                    path_white: cli.engines[j].display().to_string(),
                    label_black: engine_labels[i].clone(),
                    label_white: engine_labels[j].clone(),
                },
                start_positions: start_commands.clone(),
                output: path.display().to_string(),
            };
            pw.write_json(&meta)?;
            pw.flush()?;

            pair_writers.insert((i, j), pw);
            pair_game_count.insert((i, j), 0);
        }
    }

    // チャネルの作成（ランデブー）
    let (ticket_tx, ticket_rx) = chan::bounded::<Option<MatchTicket>>(0);
    let (result_tx, result_rx) = chan::bounded::<MatchResult>(0);

    // ワーカースレッドの起動
    let mut handles = Vec::new();
    for _ in 0..cli.concurrency {
        let engine_paths = cli.engines.clone();
        let labels = engine_labels.clone();
        let usi_opts = engine_usi_options.clone();
        let threads = cli.threads;
        let hash_mb = cli.hash_mb;
        let max_moves = cli.max_moves;
        let timeout_margin_ms = cli.timeout_margin_ms;
        let byoyomi = cli.byoyomi;
        let start_positions: Vec<ParsedPosition> = start_defs
            .iter()
            .map(|p| ParsedPosition {
                startpos: p.startpos,
                sfen: p.sfen.clone(),
                moves: p.moves.clone(),
            })
            .collect();
        let rx = ticket_rx.clone();
        let tx = result_tx.clone();
        let sd = shutdown.clone();
        handles.push(thread::spawn(move || {
            worker_main(
                WorkerConfig {
                    engine_paths,
                    engine_labels: labels,
                    engine_usi_options: usi_opts,
                    threads,
                    hash_mb,
                    max_moves,
                    timeout_margin_ms,
                    byoyomi,
                    start_positions,
                },
                rx,
                tx,
                sd,
            );
        }));
    }
    // メインスレッドは result_tx を持たないので drop
    drop(result_tx);

    // 勝敗カウンター: pair_key → (wins_i, wins_j, draws) (i < j)
    let mut pair_stats: HashMap<(usize, usize), (u32, u32, u32)> = HashMap::new();
    for i in 0..n {
        for j in (i + 1)..n {
            pair_stats.insert((i, j), (0, 0, 0));
        }
    }

    let start_time = Instant::now();
    let mut completed = 0u32;
    let mut ticket_iter = tickets.into_iter();
    let mut next_ticket: Option<MatchTicket> = ticket_iter.next();

    // メインイベントループ
    while completed < total_games && !shutdown.load(Ordering::Relaxed) {
        match &next_ticket {
            None => {
                // チケットは全送信済み、結果を待つ
                match result_rx.recv() {
                    Ok(result) => {
                        process_result(
                            &result,
                            &engine_labels,
                            &mut pair_writers,
                            &mut pair_stats,
                            &mut pair_game_count,
                        )?;
                        completed += 1;
                        if completed.is_multiple_of(cli.report_interval) || completed == total_games
                        {
                            print_progress(
                                completed,
                                total_games,
                                &pair_stats,
                                &engine_labels,
                                start_time,
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
            Some(t) => {
                chan::select! {
                    send(ticket_tx, Some(t.clone())) -> res => {
                        if res.is_ok() {
                            next_ticket = ticket_iter.next();
                        }
                    }
                    recv(result_rx) -> result => {
                        if let Ok(result) = result {
                            process_result(
                                &result,
                                &engine_labels,
                                &mut pair_writers,
                                &mut pair_stats,
                                &mut pair_game_count,
                            )?;
                            completed += 1;
                            if completed.is_multiple_of(cli.report_interval) || completed == total_games {
                                print_progress(
                                    completed,
                                    total_games,
                                    &pair_stats,
                                    &engine_labels,
                                    start_time,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // ワーカーを停止
    for _ in 0..cli.concurrency {
        let _ = ticket_tx.send(None);
    }
    for h in handles {
        let _ = h.join();
    }

    // ライターをフラッシュ
    for (_, pw) in pair_writers.iter_mut() {
        pw.flush()?;
    }

    println!();
    println!("=== Tournament Complete ===");
    println!("Total: {} games in {:.1}s", completed, start_time.elapsed().as_secs_f64());
    print_final_table(&pair_stats, &engine_labels);
    println!("Output: {}", cli.out_dir.display());
    println!("===========================");
    Ok(())
}

// ---------------------------------------------------------------------------
// ヘルパー
// ---------------------------------------------------------------------------

fn process_result(
    result: &MatchResult,
    engine_labels: &[String],
    pair_writers: &mut HashMap<(usize, usize), PairWriter>,
    pair_stats: &mut HashMap<(usize, usize), (u32, u32, u32)>,
    pair_game_count: &mut HashMap<(usize, usize), u32>,
) -> Result<()> {
    let bi = result.ticket.black_idx;
    let wi = result.ticket.white_idx;
    let pair_key = if bi < wi { (bi, wi) } else { (wi, bi) };

    // ゲーム番号をペアごとに採番
    let game_num = pair_game_count.entry(pair_key).or_insert(0);
    *game_num += 1;
    let game_id = *game_num;

    // ファイルに書き出し
    if let Some(pw) = pair_writers.get_mut(&pair_key) {
        for ml in &result.move_logs {
            // game_id をペアローカルのものに書き換え
            let entry = MoveLogEntry {
                kind: ml.kind,
                game_id,
                ply: ml.ply,
                side_to_move: ml.side_to_move,
                sfen_before: ml.sfen_before.clone(),
                move_usi: ml.move_usi.clone(),
                raw_move_usi: ml.raw_move_usi.clone(),
                engine: ml.engine.clone(),
                elapsed_ms: ml.elapsed_ms,
                think_limit_ms: ml.think_limit_ms,
                timed_out: ml.timed_out,
                eval: ml.eval.clone(),
            };
            pw.write_json(&entry)?;
        }
        let winner = match result.outcome {
            GameOutcome::BlackWin => Some(engine_labels[result.ticket.black_idx].clone()),
            GameOutcome::WhiteWin => Some(engine_labels[result.ticket.white_idx].clone()),
            GameOutcome::Draw | GameOutcome::InProgress => None,
        };
        let result_entry = ResultLogEntry {
            kind: "result",
            game_id,
            outcome: result.outcome.label(),
            reason: &result.reason,
            plies: result.plies,
            winner,
        };
        pw.write_json(&result_entry)?;
        pw.flush()?;
    }

    // 統計更新
    if let Some(stats) = pair_stats.get_mut(&pair_key) {
        match result.outcome {
            GameOutcome::BlackWin => {
                if bi == pair_key.0 {
                    stats.0 += 1; // i wins
                } else {
                    stats.1 += 1; // j wins
                }
            }
            GameOutcome::WhiteWin => {
                if wi == pair_key.0 {
                    stats.0 += 1;
                } else {
                    stats.1 += 1;
                }
            }
            GameOutcome::Draw | GameOutcome::InProgress => {
                stats.2 += 1;
            }
        }
    }

    Ok(())
}

fn print_progress(
    completed: u32,
    total: u32,
    pair_stats: &HashMap<(usize, usize), (u32, u32, u32)>,
    engine_labels: &[String],
    start_time: Instant,
) {
    let elapsed = start_time.elapsed().as_secs_f64();
    let gps = if elapsed > 0.0 {
        completed as f64 / elapsed
    } else {
        0.0
    };
    println!(
        "\n--- Progress: {}/{} ({:.1}%) | {:.1} games/sec ---",
        completed,
        total,
        completed as f64 / total as f64 * 100.0,
        gps
    );
    for (&(i, j), &(wi, wj, d)) in pair_stats {
        let total_pair = wi + wj + d;
        if total_pair == 0 {
            continue;
        }
        let li = &engine_labels[i];
        let lj = &engine_labels[j];
        let wr = if total_pair > 0 {
            (wi as f64 + d as f64 * 0.5) / total_pair as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  {} vs {}: {}W-{}L-{}D ({} games, {} win rate: {:.1}%)",
            li, lj, wi, wj, d, total_pair, li, wr
        );
    }
}

fn print_final_table(
    pair_stats: &HashMap<(usize, usize), (u32, u32, u32)>,
    engine_labels: &[String],
) {
    println!();
    for (&(i, j), &(wi, wj, d)) in pair_stats {
        let total_pair = wi + wj + d;
        if total_pair == 0 {
            continue;
        }
        let li = &engine_labels[i];
        let lj = &engine_labels[j];
        let score_i = wi as f64 + d as f64 * 0.5;
        let wr = score_i / total_pair as f64;
        let elo = if wr > 0.0 && wr < 1.0 {
            Some(-400.0 * (1.0 / wr - 1.0).log10())
        } else {
            None
        };
        let elo_str = elo.map_or("N/A".to_string(), |e| format!("{:+.0}", e));
        println!(
            "  {} vs {}: {}W-{}L-{}D | {} win rate: {:.1}% | Elo: {}",
            li,
            lj,
            wi,
            wj,
            d,
            li,
            wr * 100.0,
            elo_str
        );
    }
}

fn engine_label_from_path(path: &Path) -> String {
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown");
    // rshogi-usi-HASH パターンからハッシュ部分を抽出
    if let Some(rest) = filename.strip_prefix("rshogi-usi-") {
        let hash: String = rest.chars().take(8).collect();
        if !hash.is_empty() {
            return hash;
        }
    }
    filename.to_string()
}
