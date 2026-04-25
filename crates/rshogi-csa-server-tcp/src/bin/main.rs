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
use rshogi_csa_server::error::StorageError;
use rshogi_csa_server::port::{PlayerRateRecord, RateStorage};
use rshogi_csa_server::types::PlayerName;
use rshogi_csa_server::{ClockSpec, FileKifuStorage};
use rshogi_csa_server_tcp::auth::PlainPasswordHasher;
use rshogi_csa_server_tcp::broadcaster::InMemoryBroadcaster;
use rshogi_csa_server_tcp::rate_limit::IpLoginRateLimiter;
use rshogi_csa_server_tcp::server::{
    InMemoryPasswordStore, ServerConfig, build_state, prepare_runtime, run_server,
};
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
    /// 秒読み方式 / Fischer 方式で使う持ち時間 (秒)。
    #[arg(long, default_value_t = 600)]
    total_time_sec: u32,
    /// 秒読み方式の秒読み、または Fischer 方式の増分 (秒)。
    #[arg(long, default_value_t = 10)]
    byoyomi_sec: u32,
    /// StopWatch 方式で使う持ち時間 (分)。
    #[arg(long, default_value_t = 10)]
    total_time_min: u32,
    /// StopWatch 方式の秒読み (分)。
    #[arg(long, default_value_t = 1)]
    byoyomi_min: u32,
    /// 時計方式。`countdown` / `fischer` / `stopwatch`。
    #[arg(long, value_enum, default_value_t = ClockKindArg::Countdown)]
    clock_kind: ClockKindArg,
    /// 通信マージン (ミリ秒)。
    #[arg(long, default_value_t = 1_500)]
    margin_ms: u64,
    /// 最大手数。
    #[arg(long, default_value_t = 256)]
    max_moves: u32,
    /// AGREE 受信の最大待機時間（秒）。GUI/エンジンの起動待ちを許容するため長めの既定値。
    #[arg(long, default_value_t = 300)]
    agree_timeout_sec: u64,
    /// `%%SETBUOY` / `%%DELETEBUOY` を許可する admin ハンドル。複数指定可 (例:
    /// `--admin-handle alice --admin-handle bob`)。空の場合はブイ登録コマンドを
    /// 全リクエストで `PERMISSION_DENIED` で拒否する (Codex review PR #470 3rd
    /// round P2)。`%%GETBUOYCOUNT` は参照系なので権限不要で全ユーザー可。
    #[arg(long = "admin-handle", value_name = "HANDLE")]
    admin_handle: Vec<String>,
    /// Floodgate 運用機能の opt-in フラグ。Floodgate 系機能を本バイナリで
    /// 有効化する PR は、本フラグが `true` のときだけ配線を生かすように
    /// 実装する（現時点ではまだ配線された機能は無い）。
    /// CLI 名は `prepare_runtime` 失敗時のエラーメッセージで参照する
    /// [`ALLOW_FLOODGATE_FEATURES_FLAG`] と同じ綴り。リネームする際は両方
    /// 同時に変更すること（同期は下のユニットテストが回帰検知する）。
    #[arg(long, default_value_t = false)]
    allow_floodgate_features: bool,
    /// SIGINT / SIGTERM 受信後に進行中対局の完了を待つ秒数。超過分は未完了の
    /// まま warning log を出して切り捨てる。ローリング再起動で対局を落とさない
    /// ためのバッファ。
    #[arg(long, default_value_t = 60)]
    shutdown_grace_sec: u64,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum ClockKindArg {
    Countdown,
    Fischer,
    Stopwatch,
}

impl ClockKindArg {
    fn to_clock_spec(
        self,
        total_time_sec: u32,
        byoyomi_sec: u32,
        total_time_min: u32,
        byoyomi_min: u32,
    ) -> ClockSpec {
        match self {
            Self::Countdown => ClockSpec::Countdown {
                total_time_sec,
                byoyomi_sec,
            },
            Self::Fischer => ClockSpec::Fischer {
                total_time_sec,
                increment_sec: byoyomi_sec,
            },
            Self::Stopwatch => ClockSpec::StopWatch {
                total_time_min,
                byoyomi_min,
            },
        }
    }
}

/// `Cli::allow_floodgate_features` から clap が生成する CLI フラグ名。
/// `prepare_runtime` 失敗時のエラーメッセージ生成に使う。clap derive の
/// `#[arg(long, ...)]` はフィールド名から flag を生成するため、この const と
/// フィールド名を同期させる必要がある。`flag_name_matches_field_name`
/// テストが両者の一致を回帰検知する。
const ALLOW_FLOODGATE_FEATURES_FLAG: &str = "--allow-floodgate-features";

fn main() -> anyhow::Result<()> {
    init_tracing();
    tracing::info!(version = %env!("CARGO_PKG_VERSION"), "rshogi-csa-server-tcp starting");

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
        clock: cli.clock_kind.to_clock_spec(
            cli.total_time_sec,
            cli.byoyomi_sec,
            cli.total_time_min,
            cli.byoyomi_min,
        ),
        time_margin_ms: cli.margin_ms,
        max_moves: cli.max_moves,
        login_timeout: std::time::Duration::from_secs(30),
        agree_timeout: std::time::Duration::from_secs(cli.agree_timeout_sec),
        x1_reply_write_timeout: std::time::Duration::from_secs(5),
        entering_king_rule: rshogi_core::types::EnteringKingRule::Point24,
        initial_sfen: None,
        admin_handles: cli.admin_handle.clone(),
        allow_floodgate_features: cli.allow_floodgate_features,
        shutdown_grace: std::time::Duration::from_secs(cli.shutdown_grace_sec),
    };
    // Floodgate 系機能の opt-in ゲートを起動前に評価する。要求があるのに
    // フラグが立っていない場合はここで起動を止める。
    prepare_runtime(&config).map_err(|msg| {
        anyhow::anyhow!(
            "{msg}; pass {ALLOW_FLOODGATE_FEATURES_FLAG} to enable Floodgate runtime features",
        )
    })?;

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
        let handle = run_server(state.clone()).await.context("run_server")?;
        tracing::info!("rshogi-csa-server-tcp ready");

        // SIGINT と SIGTERM を並列待機する。SIGINT は Ctrl-C、SIGTERM は
        // systemd / Docker / Kubernetes の停止シグナル。
        let sig = wait_for_termination_signal().await;
        tracing::info!(signal = sig, "initiating graceful shutdown");

        // 1. 新規接続の受付停止 + 待機プール中のセッション切断を誘導する。
        state.shutdown.trigger();

        // 2. accept ループの終了を待つ。shutdown が立っているので即座に抜ける。
        //    panic した場合に listener 未解放のまま後段に進まないよう、panic は
        //    error log に落として正常経路と同様に fall-through させる。
        //    `{e:#}` は JoinError の Debug 出力（panic payload とロケーション）を
        //    一行に展開するので、運用時の原因調査で使える。
        match handle.await {
            Ok(()) => {}
            Err(e) if e.is_panic() => {
                tracing::error!(error = %format!("{e:#}"), "accept loop panicked during shutdown");
            }
            Err(e) => {
                tracing::info!(error = %format!("{e:#}"), "accept loop joined with error");
            }
        }

        // 3. 進行中対局が終局するまで待つ。grace を超過したら warning を出して
        //    プロセス終了へ進む（残りの対局タスクは LocalSet 終了で abort される）。
        //    `shutdown_grace = 0` は「grace なし = 即切り」と解釈する。
        //
        //    TOCTOU 対策として `wait_active_games_notify` を先に登録してから
        //    counter を確認する。これで登録と確認の間に `notify_waiters` が
        //    発火しても取りこぼさない。
        let grace = state.config().shutdown_grace;
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            let notified = state.wait_active_games_notify();
            let active = state.active_game_count();
            if active == 0 {
                break;
            }
            tracing::info!(
                active_games = active,
                grace_sec = grace.as_secs(),
                "waiting for active games to finish"
            );
            tokio::select! {
                _ = notified => continue,
                _ = tokio::time::sleep_until(deadline) => {
                    let remaining = state.active_game_count();
                    if remaining > 0 {
                        tracing::warn!(
                            unfinished_games = remaining,
                            "shutdown grace expired"
                        );
                    }
                    break;
                }
            }
        }

        tracing::info!("shutdown complete");
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// SIGINT / SIGTERM のどちらかを受けるまで待つ。受けたシグナル名を返す。
///
/// Unix 以外のプラットフォームでは SIGTERM 経路は `pending` になるため、
/// 実質 SIGINT (Ctrl-C) のみが反応する。
async fn wait_for_termination_signal() -> &'static str {
    let sigint = async {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    };
    #[cfg(unix)]
    let sigterm = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
                "SIGTERM"
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<&'static str>().await
            }
        }
    };
    #[cfg(not(unix))]
    let sigterm = std::future::pending::<&'static str>();
    tokio::select! {
        s = sigint => s,
        s = sigterm => s,
    }
}

/// `tracing_subscriber` の初期化。
///
/// `RUST_LOG` 互換の env-filter で level / target を設定でき、未設定時は
/// `info` レベルで rshogi-csa-server-tcp のイベントを出力する。`tracing-log`
/// ブリッジを有効化しているので、依存先 crate が `log` macro を使っていても
/// 同じ subscriber に流れて 1 系統の出力になる。
///
/// 構造化フィールド（`conn_id` / `game_id` 等）は `info!(field = value)` 形式で
/// span に乗り、`fmt` フォーマッタが key=value で展開する。日次ローテはここでは
/// 設定せず、systemd journal / Docker logging driver / log shipper 等の
/// プロセス外設定に任せる（YAGNI）。
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt;
    use tracing_subscriber::prelude::*;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // ANSI カラー escape は log shipper / journald 経由で消費する運用でノイズに
    // なるため既定で off。開発者がローカルで色付きログを見たい場合は
    // `RSHOGI_LOG_ANSI=1` を立てて override できる（YAGNI ぎりぎり残す程度に）。
    let ansi = std::env::var("RSHOGI_LOG_ANSI").as_deref() == Ok("1");
    let registry = tracing_subscriber::registry().with(filter).with(fmt::layer().with_ansi(ansi));
    if registry.try_init().is_err() {
        // テスト等で既に初期化されている場合は何もしない（多重 init 失敗を許容）。
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// `ALLOW_FLOODGATE_FEATURES_FLAG` 定数と clap が生成する CLI フラグ名が
    /// 一致することを固定する。`Cli::allow_floodgate_features` のフィールド名を
    /// リネームしたら本テストが落ち、エラーメッセージ生成側との同期忘れを検知する。
    #[test]
    fn allow_floodgate_features_flag_matches_clap_long() {
        let cmd = Cli::command();
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_id() == "allow_floodgate_features")
            .expect("allow_floodgate_features arg must exist on Cli");
        let long = arg.get_long().expect("--allow-floodgate-features must have a long form");
        assert_eq!(
            format!("--{long}"),
            ALLOW_FLOODGATE_FEATURES_FLAG,
            "ALLOW_FLOODGATE_FEATURES_FLAG must stay in sync with clap-generated flag",
        );
    }
}
