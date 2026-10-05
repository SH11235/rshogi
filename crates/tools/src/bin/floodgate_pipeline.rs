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
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tools::common::dedup::DedupSet;
use tools::common::floodgate as fg;
use tools::common::io::{open_writer, write_atomic};
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
        /// レーティングページ URL（未指定なら直近の日付のページを自動取得）
        #[arg(long)]
        url: Option<String>,
        /// レーティング閾値（この値以上のプレイヤーを出力）
        #[arg(long, default_value_t = 3900)]
        min_rating: u32,
        /// 出力ファイルパス（1行1プレイヤー名）
        #[arg(long, default_value = "high_rated_players.txt")]
        out: String,
    },
    /// 00LIST.floodgateインデックスをダウンロード
    FetchIndex {
        /// Root URL（既定は HTTPS。http 指定時もサーバ側 301 で https へ誘導）
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
        /// Root URL（既定は HTTPS。http 指定時もサーバ側 301 で https へ誘導）
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
    /// 当日 (JST) の対局 CSA をローカル dir へミラーし続ける。進行中の対局は逐次
    /// 追記されるため間隔ごとに再取得し、終局 (`'$END_TIME:`) を検出したら以後取得
    /// しない。`kifu_player --csa <dir> --live` と組で wdoor のほぼリアルタイム観戦に
    /// 使う。停止は Ctrl-C (書き込みは tmp→rename で常に完全な内容)。
    LiveMirror {
        /// ミラー先ディレクトリ (ファイル名は wdoor のまま)
        #[arg(long)]
        out_dir: PathBuf,
        /// 進行中対局の再取得間隔 (秒)。対局一覧 (日次 index) の確認は約 60 秒ごと
        /// (interval がそれより長い場合はそのパス頻度)
        #[arg(long, default_value_t = 10)]
        interval: u64,
        /// 対局者名の部分一致フィルタ (カンマ区切り)。未指定は当日の全対局をミラー
        #[arg(long, value_delimiter = ',')]
        watch: Vec<String>,
        /// Root URL（既定は HTTPS）
        #[arg(long, default_value = fg::DEFAULT_ROOT)]
        root: String,
        /// 1 パスだけ実行して終了 (動作確認用)
        #[arg(long)]
        once: bool,
        /// wdoor MONITOR2 を着手通知として使い、通知ごとに HTTP 正本 CSA を取得する
        #[arg(long)]
        push: bool,
        /// MONITOR2 観戦ログイン名 (`--push` 時のみ使用)
        #[arg(long, default_value = "rshogi-mirror")]
        login_name: String,
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
        } => run_fetch_ratings(url.as_deref(), min_rating, &out),
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
        Cmd::LiveMirror {
            out_dir,
            interval,
            watch,
            root,
            once,
            push,
            login_name,
        } => run_live_mirror(&out_dir, interval, &watch, &root, once, push, &login_name),
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

/// live-mirror が追跡する 1 リモートファイルぶんの状態。
struct MirrorState {
    url: String,
    local: PathBuf,
    /// 直近取得の本文サイズ。CSA は追記のみで単調増加するため「同サイズ = 変化なし」。
    size: u64,
    /// 直近で本文サイズが変化した時刻。変化が長時間ない進行中対局は放棄する。
    last_changed_at: Instant,
    /// 終局検出済み(または放棄)。以後は取得しない。
    finished: bool,
    /// MONITOR2ON を送信済みか。TCP 再接続時は main loop 側で false に戻す。
    push_subscribed: bool,
}

/// 対局一覧 (日次 autoindex) の確認間隔。ペアリングは毎時 :00/:30 なので秒単位で追う
/// 意味が薄く、wdoor への負荷を抑えるため進行中ファイルの再取得とは別に長めへ固定する。
const INDEX_POLL_SECS: u64 = 60;
/// MONITOR2 接続中に `%%LIST` を再送する間隔。LIST は新規対局発見だけに使い、棋譜本文は
/// 常に HTTP 公開 CSA から取得する。
const PUSH_LIST_SECS: u64 = 30;
/// MONITOR2 接続中も HTTP ポーリングを完全には止めず、通知欠落時の安全網として使う間隔。
const PUSH_POLL_SECS: u64 = 60;
/// MONITOR2 着手通知から HTTP 取得までの遅延。短時間に複数通知が来た場合は `insert` の
/// 上書きで期限を後ろへずらし、書きかけ公開ファイルを避ける。
const PUSH_DEBOUNCE_MS: u64 = 200;
/// wdoor の CSA TCP サーバ。
const WDOOR_MONITOR_ADDR: &str = "wdoor.c.u-tokyo.ac.jp:4081";
/// MONITOR2 観戦ログインではパスワード値は認証に使われず、任意文字列でよい。
const WDOOR_MONITOR_PASSWORD: &str = "rshogi";
/// LOGIN 応答を待つ上限。タイムアウト付き read_line の WouldBlock はこの期限まで継続する。
const MONITOR2_LOGIN_TIMEOUT_SECS: u64 = 15;
/// 進行中のまま内容変化が止まった対局を放棄するまでの時間。
const STALE_GAME_SECS: u64 = 3600;

fn run_live_mirror(
    out_dir: &Path,
    interval: u64,
    watch: &[String],
    root: &str,
    once: bool,
    push: bool,
    login_name: &str,
) -> Result<()> {
    anyhow::ensure!(interval >= 1, "--interval は 1 秒以上を指定してください");
    fs::create_dir_all(out_dir).with_context(|| format!("create dir {}", out_dir.display()))?;
    // 無人常駐が前提なので、応答しないピアで単一スレッドのループ全体が
    // 止まらないようリクエストにタイムアウトを付ける。
    let client = fg::http_client_builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let mut states: std::collections::HashMap<String, MirrorState> =
        std::collections::HashMap::new();
    let mut last_index_poll: Option<Instant> = None;
    let (mut push_rx, mut push_tx) = if push && !once {
        let (event_tx, event_rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        start_monitor2_thread(login_name.to_string(), event_tx, cmd_rx);
        (Some(event_rx), Some(cmd_tx))
    } else {
        if push && once {
            eprintln!("live-mirror: --once では --push を無視して 1 パスだけ実行します");
        }
        (None, None)
    };
    let mut push_connected = false;
    let mut pending_push_fetches: HashMap<String, Instant> = HashMap::new();
    let mut last_file_poll: Option<Instant> = None;
    loop {
        // 1) 日次 index から対局を発見(INDEX_POLL_SECS ごと)。
        if last_index_poll.is_none_or(|t| t.elapsed().as_secs() >= INDEX_POLL_SECS) {
            last_index_poll = Some(Instant::now());
            match discover_from_autoindex(&client, out_dir, watch, root, &mut states) {
                Ok(added) => {
                    if added > 0 {
                        eprintln!("live-mirror: 対局発見 +{added} (追跡 {} 局)", states.len());
                    }
                    prune_old_finished(&mut states);
                }
                Err(e) => eprintln!("⚠ 対局一覧の取得失敗(次回再試行): {e:#}"),
            }
        }

        if let Some(rx) = &push_rx {
            while let Ok(event) = rx.try_recv() {
                handle_push_event(
                    event,
                    &client,
                    out_dir,
                    watch,
                    root,
                    &mut states,
                    &push_tx,
                    &mut push_connected,
                    &mut pending_push_fetches,
                );
            }
            if push_connected {
                subscribe_push_targets(&mut states, &push_tx);
            }
            flush_due_push_fetches(
                &client,
                &mut states,
                &push_tx,
                &mut pending_push_fetches,
                Instant::now(),
            );
        }

        // 2) 進行中ファイルの再取得。
        let mut updated = 0usize;
        let mut in_progress = 0usize;
        let safety_interval = if push_connected {
            interval.max(PUSH_POLL_SECS)
        } else {
            interval
        };
        let should_poll_files =
            last_file_poll.is_none_or(|t| t.elapsed().as_secs() >= safety_interval);
        if should_poll_files {
            last_file_poll = Some(Instant::now());
            for (name, st) in states.iter_mut().filter(|(_, s)| !s.finished) {
                match mirror_one(&client, st) {
                    Ok(true) => {
                        updated += 1;
                        if st.finished {
                            eprintln!("live-mirror: 終局 {}", st.local.display());
                            unsubscribe_finished_game(name, st, &push_tx);
                        }
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("⚠ 取得失敗(次回再試行) {}: {e:#}", st.url);
                    }
                }
                if !st.finished {
                    if st.last_changed_at.elapsed() >= Duration::from_secs(STALE_GAME_SECS) {
                        // 追跡だけ止める(ローカルの部分棋譜は勝敗不明として残る)。
                        st.finished = true;
                        eprintln!(
                            "⚠ {} は約 1 時間変化なし。中断対局とみなし追跡を止める",
                            st.local.display()
                        );
                        unsubscribe_finished_game(name, st, &push_tx);
                    } else {
                        in_progress += 1;
                    }
                }
            }
            if updated > 0 {
                eprintln!(
                    "live-mirror: 更新 {updated} 局 (進行中 {in_progress} / 追跡 {} 局)",
                    states.len()
                );
            }
            if push_connected {
                subscribe_push_targets(&mut states, &push_tx);
            }
        }
        if once {
            break;
        }
        let file_poll_wait = last_file_poll
            .map(|t| Duration::from_secs(safety_interval).saturating_sub(t.elapsed()))
            .unwrap_or_default();
        let push_wait = pending_push_fetches
            .values()
            .min()
            .map(|due| due.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|| Duration::from_secs(safety_interval));
        let wait = file_poll_wait.min(push_wait);
        if let Some(rx) = &push_rx {
            match rx.recv_timeout(wait) {
                Ok(event) => {
                    handle_push_event(
                        event,
                        &client,
                        out_dir,
                        watch,
                        root,
                        &mut states,
                        &push_tx,
                        &mut push_connected,
                        &mut pending_push_fetches,
                    );
                    while let Ok(event) = rx.try_recv() {
                        handle_push_event(
                            event,
                            &client,
                            out_dir,
                            watch,
                            root,
                            &mut states,
                            &push_tx,
                            &mut push_connected,
                            &mut pending_push_fetches,
                        );
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    push_connected = false;
                    push_rx = None;
                    push_tx = None;
                }
            }
        } else {
            std::thread::sleep(Duration::from_secs(interval));
        }
    }
    Ok(())
}

fn discover_from_autoindex(
    client: &Client,
    out_dir: &Path,
    watch: &[String],
    root: &str,
    states: &mut HashMap<String, MirrorState>,
) -> Result<usize> {
    let day_url = fg::day_dir_url(root, fg::jst_today())?;
    let html = fg::http_get_text(client, &day_url)?;
    let mut added = 0usize;
    for name in fg::parse_autoindex_csa_names(&html) {
        let url = format!("{day_url}{name}");
        if add_mirror_state(out_dir, watch, states, name, url) {
            added += 1;
        }
    }
    Ok(added)
}

fn add_mirror_state(
    out_dir: &Path,
    watch: &[String],
    states: &mut HashMap<String, MirrorState>,
    name: String,
    url: String,
) -> bool {
    if !watch.is_empty() && !watch.iter().any(|w| name.contains(w.as_str())) {
        return false;
    }
    if states.contains_key(&name) {
        return false;
    }
    // 過去の実行で完全な棋譜をミラー済みなら取得しない。
    let local = out_dir.join(&name);
    let finished = fs::read_to_string(&local).map(|t| fg::csa_is_finished(&t)).unwrap_or(false);
    let size = fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
    states.insert(
        name,
        MirrorState {
            url,
            local,
            size,
            last_changed_at: Instant::now(),
            finished,
            push_subscribed: false,
        },
    );
    true
}

fn prune_old_finished(states: &mut HashMap<String, MirrorState>) {
    // 終局済みで前日以前の対局は追跡から外す(常駐時のメモリじわ増防止)。
    // 当日 index には現れないので再発見されず、仮に再発見されても
    // ローカルの完全棋譜チェックが再取得を防ぐ。
    let today = fg::jst_today().format("%Y%m%d").to_string();
    states.retain(|name, st| {
        !(st.finished && csa_name_date(name).is_some_and(|d| d < today.as_str()))
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Monitor2Payload {
    Move,
    SpecialMove,
    Result,
    Comment,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Monitor2Line {
    game_id: String,
    kind: Monitor2Payload,
}

#[derive(Debug)]
enum PushEvent {
    Connected,
    Disconnected(String),
    ListGame(String),
    Monitor2(Monitor2Line),
}

#[derive(Debug)]
enum PushCommand {
    MonitorOn(String),
    MonitorOff(String),
}

fn handle_push_event(
    event: PushEvent,
    client: &Client,
    out_dir: &Path,
    watch: &[String],
    root: &str,
    states: &mut HashMap<String, MirrorState>,
    push_tx: &Option<mpsc::Sender<PushCommand>>,
    push_connected: &mut bool,
    pending_fetches: &mut HashMap<String, Instant>,
) {
    match event {
        PushEvent::Connected => {
            *push_connected = true;
            for st in states.values_mut() {
                st.push_subscribed = false;
            }
            eprintln!("live-mirror: MONITOR2 接続完了");
        }
        PushEvent::Disconnected(reason) => {
            *push_connected = false;
            pending_fetches.clear();
            for st in states.values_mut() {
                st.push_subscribed = false;
            }
            eprintln!("⚠ MONITOR2 接続断。ポーリングへフォールバックします: {reason}");
        }
        PushEvent::ListGame(game_id) => match csa_url_from_game_id(root, &game_id) {
            Ok(url) => {
                let name = format!("{game_id}.csa");
                if add_mirror_state(out_dir, watch, states, name.clone(), url) {
                    pending_fetches.insert(name, Instant::now());
                    eprintln!("live-mirror: MONITOR2 LIST 対局発見 +1 (追跡 {} 局)", states.len());
                }
            }
            Err(e) => eprintln!("⚠ MONITOR2 LIST の game_id を URL 化できません: {game_id}: {e:#}"),
        },
        PushEvent::Monitor2(line) => match line.kind {
            Monitor2Payload::Move | Monitor2Payload::SpecialMove | Monitor2Payload::Result => {
                let name = format!("{}.csa", line.game_id);
                pending_fetches
                    .insert(name, Instant::now() + Duration::from_millis(PUSH_DEBOUNCE_MS));
            }
            Monitor2Payload::Comment | Monitor2Payload::Other => {}
        },
    }
    if *push_connected {
        subscribe_push_targets(states, push_tx);
    }
    flush_due_push_fetches(client, states, push_tx, pending_fetches, Instant::now());
}

fn subscribe_push_targets(
    states: &mut HashMap<String, MirrorState>,
    push_tx: &Option<mpsc::Sender<PushCommand>>,
) {
    let Some(tx) = push_tx else { return };
    for (name, st) in states
        .iter_mut()
        .filter(|(_, st)| st.size > 0 && !st.finished && !st.push_subscribed)
    {
        let game_id = name.strip_suffix(".csa").unwrap_or(name).to_string();
        if tx.send(PushCommand::MonitorOn(game_id)).is_ok() {
            st.push_subscribed = true;
        }
    }
}

fn unsubscribe_finished_game(
    name: &str,
    st: &mut MirrorState,
    push_tx: &Option<mpsc::Sender<PushCommand>>,
) {
    if !st.push_subscribed {
        return;
    }
    st.push_subscribed = false;
    if let Some(tx) = push_tx {
        let game_id = name.strip_suffix(".csa").unwrap_or(name).to_string();
        let _ = tx.send(PushCommand::MonitorOff(game_id));
    }
}

fn flush_due_push_fetches(
    client: &Client,
    states: &mut HashMap<String, MirrorState>,
    push_tx: &Option<mpsc::Sender<PushCommand>>,
    pending_fetches: &mut HashMap<String, Instant>,
    now: Instant,
) {
    let due_names: Vec<String> = pending_fetches
        .iter()
        .filter(|(_, due)| **due <= now)
        .map(|(name, _)| name.clone())
        .collect();
    for name in due_names {
        pending_fetches.remove(&name);
        let Some(st) = states.get_mut(&name) else {
            continue;
        };
        if st.finished {
            continue;
        }
        match mirror_one(client, st) {
            Ok(true) => {
                eprintln!("live-mirror: push 更新 {}", st.local.display());
                if st.finished {
                    eprintln!("live-mirror: 終局 {}", st.local.display());
                    unsubscribe_finished_game(&name, st, push_tx);
                }
            }
            Ok(false) => {}
            Err(e) => eprintln!("⚠ push 通知後の取得失敗(次回再試行) {}: {e:#}", st.url),
        }
    }
}

fn start_monitor2_thread(
    login_name: String,
    event_tx: mpsc::Sender<PushEvent>,
    cmd_rx: mpsc::Receiver<PushCommand>,
) {
    std::thread::spawn(move || {
        let mut backoff = Duration::from_secs(5);
        let mut subscribed = HashSet::new();
        loop {
            match run_monitor2_session(
                &login_name,
                &event_tx,
                &cmd_rx,
                &mut subscribed,
                &mut backoff,
            ) {
                Ok(()) => {
                    let _ = event_tx.send(PushEvent::Disconnected(
                        "MONITOR2 セッションが正常終了しました".to_string(),
                    ));
                }
                Err(e) => {
                    let _ = event_tx.send(PushEvent::Disconnected(format!("{e:#}")));
                }
            }
            subscribed.clear();
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    });
}

fn run_monitor2_session(
    login_name: &str,
    event_tx: &mpsc::Sender<PushEvent>,
    cmd_rx: &mpsc::Receiver<PushCommand>,
    subscribed: &mut HashSet<String>,
    backoff: &mut Duration,
) -> Result<()> {
    let stream = TcpStream::connect(WDOOR_MONITOR_ADDR)
        .with_context(|| format!("connect {WDOOR_MONITOR_ADDR}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = stream.try_clone().context("MONITOR2 writer clone")?;
    writeln!(writer, "LOGIN {login_name} {WDOOR_MONITOR_PASSWORD} x1")?;
    writer.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let login_deadline = Instant::now() + Duration::from_secs(MONITOR2_LOGIN_TIMEOUT_SECS);
    loop {
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                anyhow::ensure!(Instant::now() < login_deadline, "MONITOR2 login timeout");
                continue;
            }
            Err(e) => return Err(e).context("MONITOR2 login read"),
        };
        anyhow::ensure!(n > 0, "login 中に接続が閉じました");
        if !line.ends_with('\n') {
            anyhow::ensure!(Instant::now() < login_deadline, "MONITOR2 login timeout");
            continue;
        }
        let s = line.trim_end_matches(['\r', '\n']);
        if s == format!("LOGIN:{login_name} OK") || s == "##[LOGIN] +OK x1" {
            break;
        }
        anyhow::ensure!(!is_monitor2_login_failure(s), "MONITOR2 ログイン失敗: {s}");
        line.clear();
    }
    *backoff = Duration::from_secs(5);
    let _ = event_tx.send(PushEvent::Connected);
    line.clear();
    writeln!(writer, "%%LIST")?;
    writer.flush()?;
    let mut next_list = Instant::now() + Duration::from_secs(PUSH_LIST_SECS);
    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            send_push_command(&mut writer, cmd, subscribed)?;
        }
        if Instant::now() >= next_list {
            writeln!(writer, "%%LIST")?;
            writer.flush()?;
            next_list = Instant::now() + Duration::from_secs(PUSH_LIST_SECS);
        }
        match reader.read_line(&mut line) {
            Ok(0) => anyhow::bail!("MONITOR2 peer closed"),
            Ok(_) => {
                if !line.ends_with('\n') {
                    continue;
                }
                let s = line.trim_end_matches(['\r', '\n']);
                if let Some(game_id) = parse_list_line(s) {
                    let _ = event_tx.send(PushEvent::ListGame(game_id));
                } else if let Some(mon) = parse_monitor2_line(s) {
                    let _ = event_tx.send(PushEvent::Monitor2(mon));
                }
                line.clear();
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e).context("MONITOR2 read"),
        }
    }
}

fn send_push_command(
    writer: &mut TcpStream,
    cmd: PushCommand,
    subscribed: &mut HashSet<String>,
) -> Result<()> {
    match cmd {
        PushCommand::MonitorOn(game_id) => {
            if subscribed.insert(game_id.clone()) {
                writeln!(writer, "%%MONITOR2ON {game_id}")?;
                writer.flush()?;
            }
        }
        PushCommand::MonitorOff(game_id) => {
            subscribed.remove(&game_id);
            writeln!(writer, "%%MONITOR2OFF {game_id}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

fn is_monitor2_login_failure(line: &str) -> bool {
    line.starts_with("LOGIN:incorrect")
        || line.starts_with("##[LOGIN] -NG")
        || line.starts_with("##[LOGIN] -ERR")
}

fn parse_list_line(line: &str) -> Option<String> {
    line.strip_prefix("##[LIST] ")
        .filter(|payload| *payload != "+OK")
        .map(str::to_string)
}

fn parse_monitor2_line(line: &str) -> Option<Monitor2Line> {
    let rest = line.strip_prefix("##[MONITOR2][")?;
    let (game_id, payload) = rest.split_once("] ")?;
    let kind = classify_monitor2_payload(payload);
    Some(Monitor2Line {
        game_id: game_id.to_string(),
        kind,
    })
}

fn classify_monitor2_payload(payload: &str) -> Monitor2Payload {
    if payload.starts_with('\'') {
        return Monitor2Payload::Comment;
    }
    if payload.starts_with('#') {
        return Monitor2Payload::Result;
    }
    if payload.starts_with('%') {
        return Monitor2Payload::SpecialMove;
    }
    // wdoor は着手と消費時間を別行 (`+7776FU` / `T22`) で流すが、CSA 一般には
    // `+7776FU,T0` の 1 行形式もある (本リポジトリの CSA サーバー broadcast がこの形式)。
    // どちらでも着手と判定できるよう、先頭カンマ以降は無視する。
    let head = payload.split(',').next().unwrap_or(payload);
    if is_csa_move(head) {
        return Monitor2Payload::Move;
    }
    Monitor2Payload::Other
}

fn is_csa_move(payload: &str) -> bool {
    let bytes = payload.as_bytes();
    bytes.len() == 7
        && matches!(bytes[0], b'+' | b'-')
        && bytes[1..5].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_uppercase)
}

fn csa_url_from_game_id(root: &str, game_id: &str) -> Result<String> {
    let timestamp = game_id.rsplit('+').next().context("game_id にタイムスタンプがありません")?;
    anyhow::ensure!(
        timestamp.len() == 14 && timestamp.bytes().all(|b| b.is_ascii_digit()),
        "game_id 末尾が YYYYMMDDhhmmss ではありません: {game_id}"
    );
    let rel =
        format!("{}/{}/{}/{}.csa", &timestamp[0..4], &timestamp[4..6], &timestamp[6..8], game_id);
    fg::join_url(root, &rel)
}

/// wdoor ファイル名 (`...+YYYYMMDDHHMMSS.csa`) から日付部 (`YYYYMMDD`) を取り出す。
fn csa_name_date(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".csa")?;
    let tail = stem.rsplit('+').next()?;
    (tail.len() == 14 && tail.bytes().all(|b| b.is_ascii_digit())).then(|| &tail[..8])
}

/// 1 ファイルを取得してローカルへ反映する。書いたら `Ok(true)`。
/// 条件付き GET (`If-Modified-Since`) は使わない: `Last-Modified` は秒精度で、
/// 前回取得と同じ秒内の追記(最終手や `'$END_TIME:` を含みうる)が 304 に化けて
/// 恒久的に取り逃がされるため。CSA 本文は数十 KB なので毎回取得し、追記のみで
/// 単調増加する性質から「同サイズ = 変化なし」で書き込みだけを省く。
fn mirror_one(client: &Client, st: &mut MirrorState) -> Result<bool> {
    let body = fg::http_get_text(client, &st.url)?;
    if body.len() as u64 == st.size {
        return Ok(false);
    }
    write_atomic(&st.local, &body)?;
    st.size = body.len() as u64;
    st.last_changed_at = Instant::now();
    st.finished = fg::csa_is_finished(&body);
    Ok(true)
}

fn run_fetch_ratings(url: Option<&str>, min_rating: u32, out: &str) -> Result<()> {
    let client = fg::http_client_builder().build()?;
    let html = match url {
        Some(u) => {
            eprintln!("Fetching rating page from: {u}");
            fg::http_get_text(&client, u)?
        }
        None => {
            let (u, _date, html) = fg::fetch_latest_rating_page(&client)?;
            eprintln!("Fetched latest rating page: {u}");
            html
        }
    };
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
    let client = fg::http_client_builder().build()?;
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
            static CLIENT: Client = fg::http_client_builder().build().expect("reqwest client");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csa_name_date_extracts_wdoor_date() {
        assert_eq!(
            csa_name_date("wdoor+floodgate-300-10F+A+B+20260707013003.csa"),
            Some("20260707")
        );
        assert_eq!(csa_name_date("wdoor+floodgate-300-10F+A+B+2026070701.csa"), None);
        assert_eq!(csa_name_date("no_date.csa"), None);
    }

    #[test]
    fn monitor2_line_parses_move_like_payloads() {
        let game_id = "wdoor+floodgate-300-10F+A+B+20260708000000";
        let mv = parse_monitor2_line(&format!("##[MONITOR2][{game_id}] +7776FU")).unwrap();
        assert_eq!(mv.game_id, game_id);
        assert_eq!(mv.kind, Monitor2Payload::Move);

        let drop = parse_monitor2_line(&format!("##[MONITOR2][{game_id}] -0055KA")).unwrap();
        assert_eq!(drop.kind, Monitor2Payload::Move);

        // 消費時間サフィックス付きの 1 行形式 (`+7776FU,T0`) も着手として扱う。
        let timed = parse_monitor2_line(&format!("##[MONITOR2][{game_id}] +7776FU,T0")).unwrap();
        assert_eq!(timed.kind, Monitor2Payload::Move);

        let special = parse_monitor2_line(&format!("##[MONITOR2][{game_id}] %TORYO")).unwrap();
        assert_eq!(special.kind, Monitor2Payload::SpecialMove);

        let timed_special =
            parse_monitor2_line(&format!("##[MONITOR2][{game_id}] %TORYO,T5")).unwrap();
        assert_eq!(timed_special.kind, Monitor2Payload::SpecialMove);
    }

    #[test]
    fn monitor2_line_parses_result_comment_and_other() {
        let game_id = "wdoor+floodgate-300-10F+A+B+20260708000000";
        let result = parse_monitor2_line(&format!("##[MONITOR2][{game_id}] #RESIGN")).unwrap();
        assert_eq!(result.kind, Monitor2Payload::Result);

        let comment = parse_monitor2_line(&format!("##[MONITOR2][{game_id}] '** 123")).unwrap();
        assert_eq!(comment.kind, Monitor2Payload::Comment);

        let ok = parse_monitor2_line(&format!("##[MONITOR2][{game_id}] +OK")).unwrap();
        assert_eq!(ok.kind, Monitor2Payload::Other);

        let time = parse_monitor2_line(&format!("##[MONITOR2][{game_id}] T12")).unwrap();
        assert_eq!(time.kind, Monitor2Payload::Other);
    }

    #[test]
    fn monitor2_list_and_game_id_url_parse() {
        assert_eq!(
            parse_list_line("##[LIST] wdoor+floodgate-300-10F+A+B+20260708000000").unwrap(),
            "wdoor+floodgate-300-10F+A+B+20260708000000"
        );
        assert_eq!(parse_list_line("##[LIST] +OK"), None);

        let url =
            csa_url_from_game_id(fg::DEFAULT_ROOT, "wdoor+floodgate-300-10F+A+B+20260708000000")
                .unwrap();
        assert_eq!(
            url,
            "https://wdoor.c.u-tokyo.ac.jp/shogi/x/2026/07/08/wdoor+floodgate-300-10F+A+B+20260708000000.csa"
        );
    }

    #[test]
    fn monitor2_login_failure_uses_response_prefix() {
        assert!(is_monitor2_login_failure("LOGIN:incorrect password"));
        assert!(is_monitor2_login_failure("##[LOGIN] -NG x1"));
        assert!(!is_monitor2_login_failure("banner LOGIN NG is not a login response"));
    }

    #[test]
    fn push_subscribe_requires_fetched_unfinished_game() {
        let (tx, rx) = mpsc::channel();
        let mut states = HashMap::new();
        states.insert(
            "unfetched.csa".to_string(),
            MirrorState {
                url: "https://example.invalid/unfetched.csa".to_string(),
                local: PathBuf::from("unfetched.csa"),
                size: 0,
                last_changed_at: Instant::now(),
                finished: false,
                push_subscribed: false,
            },
        );
        states.insert(
            "finished.csa".to_string(),
            MirrorState {
                url: "https://example.invalid/finished.csa".to_string(),
                local: PathBuf::from("finished.csa"),
                size: 10,
                last_changed_at: Instant::now(),
                finished: true,
                push_subscribed: false,
            },
        );
        states.insert(
            "active.csa".to_string(),
            MirrorState {
                url: "https://example.invalid/active.csa".to_string(),
                local: PathBuf::from("active.csa"),
                size: 10,
                last_changed_at: Instant::now(),
                finished: false,
                push_subscribed: false,
            },
        );

        subscribe_push_targets(&mut states, &Some(tx));

        match rx.try_recv().unwrap() {
            PushCommand::MonitorOn(game_id) => assert_eq!(game_id, "active"),
            cmd => panic!("unexpected command: {cmd:?}"),
        }
        assert!(rx.try_recv().is_err());
        assert!(!states["unfetched.csa"].push_subscribed);
        assert!(!states["finished.csa"].push_subscribed);
        assert!(states["active.csa"].push_subscribed);
    }

    #[test]
    fn unsubscribe_finished_game_sends_monitor_off_once() {
        let (tx, rx) = mpsc::channel();
        let mut st = MirrorState {
            url: "https://example.invalid/active.csa".to_string(),
            local: PathBuf::from("active.csa"),
            size: 10,
            last_changed_at: Instant::now(),
            finished: true,
            push_subscribed: true,
        };

        unsubscribe_finished_game("active.csa", &mut st, &Some(tx));

        match rx.try_recv().unwrap() {
            PushCommand::MonitorOff(game_id) => assert_eq!(game_id, "active"),
            cmd => panic!("unexpected command: {cmd:?}"),
        }
        assert!(!st.push_subscribed);
        assert!(rx.try_recv().is_err());
    }
}
