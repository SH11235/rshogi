//! rshogi-csa-server-tcp バイナリのエントリポイント。
//!
//! 最小構成として、設定ファイル無しでも以下の条件で起動できる:
//!
//! ```bash
//! cargo run -p rshogi-csa-server-tcp -- --bind 127.0.0.1:4081 --kifu-dir ./kifu \
//!     --players ./players.toml
//! ```
//!
//! `players.toml` は shogi-server の players.yaml に相当する最小形式で、
//! `<handle>` ごとに `password` / `rate` / `wins` / `losses` を持つ。
//! 書き戻しは未対応（再起動時の状態はファイルから再構築する）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context;
use clap::Parser;
use rshogi_csa_server::FileKifuStorage;
use rshogi_csa_server::error::StorageError;
use rshogi_csa_server::port::{PlayerRateRecord, RateStorage};
use rshogi_csa_server::types::PlayerName;
use rshogi_csa_server_tcp::auth::PlainPasswordHasher;
use rshogi_csa_server_tcp::broadcaster::InMemoryBroadcaster;
use rshogi_csa_server_tcp::rate_limit::IpLoginRateLimiter;
use rshogi_csa_server_tcp::server::{InMemoryPasswordStore, ServerConfig, build_state, run_server};
use tokio::sync::Mutex;

/// rshogi-csa-server-tcp CLI 引数。
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "rshogi CSA protocol shogi server (TCP frontend)",
    long_about = None,
)]
struct Cli {
    /// bind アドレス（例: `127.0.0.1:4081`）。
    #[arg(long, default_value = "127.0.0.1:4081")]
    bind: String,
    /// 棋譜と 00LIST を保存するルートディレクトリ。
    #[arg(long, default_value = "./kifu")]
    kifu_dir: PathBuf,
    /// プレイヤ定義ファイル（TOML 形式、keys = handle）。
    #[arg(long)]
    players: PathBuf,
    /// 持ち時間 (秒)。
    #[arg(long, default_value_t = 600)]
    total_time_sec: u32,
    /// 秒読み (秒)。
    #[arg(long, default_value_t = 10)]
    byoyomi_sec: u32,
    /// 通信マージン (ミリ秒)。
    #[arg(long, default_value_t = 1_500)]
    margin_ms: u64,
    /// 最大手数。
    #[arg(long, default_value_t = 256)]
    max_moves: u32,
    /// AGREE 受信の最大待機時間（秒）。GUI/エンジンの起動待ちを許容するため長めの既定値。
    #[arg(long, default_value_t = 300)]
    agree_timeout_sec: u64,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("rshogi-csa-server-tcp starting (v{})", env!("CARGO_PKG_VERSION"));

    let cli = Cli::parse();
    let bind_addr = cli.bind.parse().with_context(|| format!("bad --bind {}", cli.bind))?;

    // 1. プレイヤ定義ファイルを読む。TOML の `[players.<handle>]` エントリで表現する。
    let (password_map, rate_map) = load_players_toml(&cli.players)
        .with_context(|| format!("failed to load players file {:?}", cli.players))?;
    let password_store = InMemoryPasswordStore { map: password_map };
    let rate_storage = InMemoryRateStorage::new(rate_map);

    // 2. ServerConfig を構築。
    let config = ServerConfig {
        bind_addr,
        kifu_topdir: cli.kifu_dir,
        total_time_sec: cli.total_time_sec,
        byoyomi_sec: cli.byoyomi_sec,
        time_margin_ms: cli.margin_ms,
        max_moves: cli.max_moves,
        login_timeout: std::time::Duration::from_secs(30),
        agree_timeout: std::time::Duration::from_secs(cli.agree_timeout_sec),
        x1_reply_write_timeout: std::time::Duration::from_secs(5),
        entering_king_rule: rshogi_core::types::EnteringKingRule::Point24,
        initial_sfen: None,
        admin_handles: Vec::new(),
    };
    let kifu_storage = FileKifuStorage::new(config.kifu_topdir.clone());
    let state = Rc::new(build_state(
        config,
        rate_storage,
        kifu_storage,
        password_store,
        Box::new(PlainPasswordHasher::new()),
        IpLoginRateLimiter::default_limits(),
        InMemoryBroadcaster::new(),
    ));

    // 3. port trait の `async fn in trait` は `Send` 境界を持たないため、TCP バイナリは
    //    `current_thread` ランタイム + `LocalSet` 経路で配線する（設計方針）。
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let handle = run_server(state).await.context("run_server")?;
        log::info!("rshogi-csa-server-tcp ready");
        // 暫定のシャットダウン経路: SIGINT のみ監視し、受信したら受付タスクを `abort` で
        // 強制停止する。SIGTERM（Docker / Kubernetes）は未対応で、また abort は進行中の
        // 対局タスクと棋譜書き込みを中途半端な状態で切り捨てる可能性がある。
        //
        // 完全な graceful shutdown を入れる場合に必要な要素:
        //   - SIGTERM も含めた終了シグナル待機
        //   - 新規接続の受付停止
        //   - 進行中対局の終局待ちおよび棋譜・00LIST の原子的 flush
        // graceful shutdown 実装時にここの `handle.abort()` は cancellation token 経路へ置き換える。
        let _ = tokio::signal::ctrl_c().await;
        log::info!("shutting down");
        handle.abort();
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// players.toml を読む。
///
/// 期待する形式:
/// ```toml
/// [players.alice]
/// password = "pw"
/// rate = 1500
/// wins = 0
/// losses = 0
/// ```
fn load_players_toml(
    path: &std::path::Path,
) -> anyhow::Result<(HashMap<String, String>, HashMap<String, PlayerRateRecord>)> {
    use serde::Deserialize;
    #[derive(Debug, Deserialize)]
    struct Entry {
        password: String,
        #[serde(default = "default_rate")]
        rate: i32,
        #[serde(default)]
        wins: u32,
        #[serde(default)]
        losses: u32,
    }
    #[derive(Debug, Deserialize)]
    struct Root {
        players: HashMap<String, Entry>,
    }
    fn default_rate() -> i32 {
        1500
    }
    let raw = std::fs::read_to_string(path)?;
    let root: Root = toml::from_str(&raw)?;
    let mut password_map = HashMap::new();
    let mut rate_map = HashMap::new();
    for (name, entry) in root.players {
        password_map.insert(name.clone(), entry.password);
        rate_map.insert(
            name.clone(),
            PlayerRateRecord {
                name: PlayerName::new(&name),
                rate: entry.rate,
                wins: entry.wins,
                losses: entry.losses,
                last_game_id: None,
                last_modified: chrono::Utc::now().to_rfc3339(),
            },
        );
    }
    Ok((password_map, rate_map))
}

/// インメモリの `RateStorage`。再起動時は players.toml から再構築する前提で、
/// 実行中の書き戻し先は持たない（永続書き戻しを付けるなら別 impl を差し込む）。
pub struct InMemoryRateStorage {
    inner: Mutex<HashMap<String, PlayerRateRecord>>,
}

impl InMemoryRateStorage {
    /// 初期マップで RateStorage を構築する。
    pub fn new(map: HashMap<String, PlayerRateRecord>) -> Self {
        Self {
            inner: Mutex::new(map),
        }
    }
}

impl RateStorage for InMemoryRateStorage {
    async fn load(&self, name: &PlayerName) -> Result<Option<PlayerRateRecord>, StorageError> {
        Ok(self.inner.lock().await.get(name.as_str()).cloned())
    }

    async fn save(&self, record: &PlayerRateRecord) -> Result<(), StorageError> {
        self.inner.lock().await.insert(record.name.as_str().to_owned(), record.clone());
        Ok(())
    }

    async fn list_all(&self) -> Result<Vec<PlayerRateRecord>, StorageError> {
        Ok(self.inner.lock().await.values().cloned().collect())
    }
}
