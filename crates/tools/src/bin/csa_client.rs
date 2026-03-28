//! CSA対局クライアント
//!
//! USIエンジンをCSAプロトコル対局サーバー（floodgate等）に接続し、
//! CLIからバックグラウンドで連続対局を実行する。
//!
//! # 使用例
//!
//! ```bash
//! # TOML設定ファイルから実行
//! cargo run -p tools --bin csa_client -- config.toml
//!
//! # CLIオプションでオーバーライド
//! cargo run -p tools --bin csa_client -- config.toml --id my_engine --ponder
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use tools::csa_client::config::CsaClientConfig;
use tools::csa_client::engine::UsiEngine;
use tools::csa_client::protocol::{CsaConnection, GameResult};
use tools::csa_client::record::save_record;
use tools::csa_client::session::run_game_session;

#[derive(Parser)]
#[command(
    name = "csa_client",
    about = "CSA対局クライアント — USIエンジンをCSAサーバーに接続"
)]
struct Cli {
    /// TOML設定ファイルのパス
    config: Option<PathBuf>,

    /// CSAサーバーホスト名
    #[arg(long)]
    host: Option<String>,

    /// CSAサーバーポート番号
    #[arg(long)]
    port: Option<u16>,

    /// ログインID
    #[arg(long)]
    id: Option<String>,

    /// パスワード
    #[arg(long)]
    password: Option<String>,

    /// USIエンジンのパス
    #[arg(long)]
    engine: Option<PathBuf>,

    /// USI_Hash サイズ (MB)
    #[arg(long)]
    hash: Option<i64>,

    /// Ponder 有効化
    #[arg(long, default_missing_value = "true", num_args = 0..=1)]
    ponder: Option<bool>,

    /// Floodgate モード
    #[arg(long, default_missing_value = "true", num_args = 0..=1)]
    floodgate: Option<bool>,

    /// Keep-alive 間隔 (秒)
    #[arg(long)]
    keep_alive: Option<u64>,

    /// 秒読みマージン (ms)
    #[arg(long)]
    margin_msec: Option<u64>,

    /// 最大対局数 (0 = 無制限)
    #[arg(long)]
    max_games: Option<u32>,

    /// ログレベル
    #[arg(long)]
    log_level: Option<String>,

    /// 棋譜保存ディレクトリ
    #[arg(long)]
    record_dir: Option<PathBuf>,

    /// USIエンジンオプション (K=V,K=V,...)
    #[arg(long)]
    options: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 設定ファイル読み込み
    let mut config = if let Some(ref path) = cli.config {
        CsaClientConfig::from_file(path)
            .with_context(|| format!("設定ファイル読み込み失敗: {}", path.display()))?
    } else {
        CsaClientConfig::default()
    };

    // 環境変数でオーバーライド
    apply_env_overrides(&mut config);

    // CLI オプションでオーバーライド（最優先）
    apply_cli_overrides(&mut config, &cli);

    config.validate()?;

    // ログ初期化
    init_logger(&config);

    log::info!("CSA対局クライアント起動");
    log::info!(
        "サーバー: {}:{} (ID: {})",
        config.server.host,
        config.server.port,
        config.server.id
    );
    log::info!("エンジン: {}", config.engine.path.display());

    // SIGINT ハンドラ
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    ctrlc::set_handler(move || {
        log::info!("終了シグナル受信。対局完了後に終了します...");
        shutdown_clone.store(true, Ordering::SeqCst);
    })?;

    // エンジン起動（ループ外で保持し再利用する）
    let mut engine = spawn_engine(&config)?;

    // メイン対局ループ
    let mut games_played: u32 = 0;
    let mut wins: u32 = 0;
    let mut losses: u32 = 0;
    let mut draws: u32 = 0;
    let mut retry_delay = Duration::from_secs(config.retry.initial_delay_sec);

    loop {
        if shutdown.load(Ordering::SeqCst) {
            log::info!("シャットダウン");
            break;
        }
        if config.game.max_games > 0 && games_played >= config.game.max_games {
            log::info!("最大対局数 ({}) に達しました", config.game.max_games);
            break;
        }

        match run_one_game(&config, &mut engine, &shutdown) {
            Ok((result, record)) => {
                // 棋譜保存
                if let Err(e) = save_record(&record, &config.record) {
                    log::error!("棋譜保存エラー: {e}");
                }

                games_played += 1;
                match result {
                    GameResult::Win => wins += 1,
                    GameResult::Lose => losses += 1,
                    GameResult::Draw => draws += 1,
                    _ => {}
                }
                log::info!(
                    "対局 #{games_played} 結果: {:?} | 通算: {wins}勝 {losses}敗 {draws}分",
                    result
                );

                // 成功したのでリトライ間隔をリセット
                retry_delay = Duration::from_secs(config.retry.initial_delay_sec);

                // 毎局再起動が有効なら再起動
                if config.game.restart_engine_every_game {
                    engine.quit();
                    engine = spawn_engine(&config)?;
                }
            }
            Err(e) => {
                log::error!("対局エラー: {e}");
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                // エラー後はエンジンを再起動（不整合な状態の可能性）
                engine.quit();
                log::info!("{}秒後にリトライ...", retry_delay.as_secs());
                std::thread::sleep(retry_delay);
                retry_delay =
                    (retry_delay * 2).min(Duration::from_secs(config.retry.max_delay_sec));
                engine = spawn_engine(&config)?;
            }
        }
    }

    engine.quit();
    log::info!("終了。合計 {games_played} 局: {wins}勝 {losses}敗 {draws}分");
    Ok(())
}

fn spawn_engine(config: &CsaClientConfig) -> Result<UsiEngine> {
    UsiEngine::spawn(
        &config.engine.path,
        &config.engine.options,
        config.game.ponder,
        Duration::from_secs(config.engine.startup_timeout_sec),
    )
}

/// 1回のゲームを実行する（接続〜対局〜切断）
fn run_one_game(
    config: &CsaClientConfig,
    engine: &mut UsiEngine,
    shutdown: &AtomicBool,
) -> Result<(GameResult, tools::csa_client::record::GameRecord)> {
    // サーバー接続
    let mut conn = CsaConnection::connect(
        &config.server.host,
        config.server.port,
        config.server.keepalive.tcp,
    )?;
    conn.login(&config.server.id, &config.server.password)?;

    // 対局実行
    let result = run_game_session(&mut conn, engine, config, shutdown);

    // エラー時は投了を試みる（NF2: 対局中のエラーは投了してから再接続）
    if result.is_err() {
        let _ = conn.send_resign();
        let _ = engine.gameover("lose");
    }

    let _ = conn.logout();
    result
}

fn apply_cli_overrides(config: &mut CsaClientConfig, cli: &Cli) {
    if let Some(ref host) = cli.host {
        config.server.host = host.clone();
    }
    if let Some(port) = cli.port {
        config.server.port = port;
    }
    if let Some(ref id) = cli.id {
        config.server.id = id.clone();
    }
    if let Some(ref pw) = cli.password {
        config.server.password = pw.clone();
    }
    if let Some(ref path) = cli.engine {
        config.engine.path = path.clone();
    }
    if let Some(hash) = cli.hash {
        config.engine.options.insert("USI_Hash".to_string(), toml::Value::Integer(hash));
    }
    if let Some(ponder) = cli.ponder {
        config.game.ponder = ponder;
    }
    if let Some(fg) = cli.floodgate {
        config.server.floodgate = fg;
    }
    if let Some(ka) = cli.keep_alive {
        config.server.keepalive.ping_interval_sec = ka;
    }
    if let Some(margin) = cli.margin_msec {
        config.time.margin_msec = margin;
    }
    if let Some(max) = cli.max_games {
        config.game.max_games = max;
    }
    if let Some(ref level) = cli.log_level {
        config.log.level = level.clone();
    }
    if let Some(ref dir) = cli.record_dir {
        config.record.dir = dir.clone();
    }
    if let Some(ref opts) = cli.options {
        for kv in opts.split(',') {
            if let Some((k, v)) = kv.split_once('=') {
                let value = if let Ok(n) = v.trim().parse::<i64>() {
                    toml::Value::Integer(n)
                } else if let Ok(b) = v.trim().parse::<bool>() {
                    toml::Value::Boolean(b)
                } else {
                    toml::Value::String(v.trim().to_string())
                };
                config.engine.options.insert(k.trim().to_string(), value);
            }
        }
    }
}

fn apply_env_overrides(config: &mut CsaClientConfig) {
    if let Ok(v) = std::env::var("CSA_HOST") {
        config.server.host = v;
    }
    if let Ok(v) = std::env::var("CSA_PORT")
        && let Ok(p) = v.parse()
    {
        config.server.port = p;
    }
    if let Ok(v) = std::env::var("CSA_ID") {
        config.server.id = v;
    }
    if let Ok(v) = std::env::var("CSA_PASSWORD") {
        config.server.password = v;
    }
}

fn init_logger(config: &CsaClientConfig) {
    use std::fs::OpenOptions;
    use std::io::Write;

    let level = match config.log.level.as_str() {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    };

    // ログファイル（設定されていれば）
    let log_file = if !config.log.dir.as_os_str().is_empty() {
        let _ = std::fs::create_dir_all(&config.log.dir);
        let path = config.log.dir.join("csa_client.log");
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(std::sync::Mutex::new)
    } else {
        None
    };
    let log_file = std::sync::Arc::new(log_file);
    let write_stdout = config.log.stdout;

    let mut builder = env_logger::Builder::new();
    builder.filter_level(level);
    builder.format(move |buf, record| {
        let ts = buf.timestamp_millis();
        let msg = format!("{ts} [{}] {}", record.level(), record.args());
        // ファイルに書く
        if let Some(ref file_mutex) = *log_file
            && let Ok(mut f) = file_mutex.lock()
        {
            let _ = writeln!(f, "{msg}");
        }
        // stdout に書く（env_logger は buf への書き込みで stdout 出力を制御）
        if write_stdout {
            writeln!(buf, "{msg}")
        } else {
            // buf に空文字を書いて空行出力を抑制
            write!(buf, "")
        }
    });
    builder.init();
}
