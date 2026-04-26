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
use rshogi_csa_server::port::{KifuStorage, PlayerRateRecord, RateStorage};
use rshogi_csa_server::types::PlayerName;
use rshogi_csa_server::{ClockSpec, FileKifuStorage, PlayersYamlRateStorage};
use rshogi_csa_server_tcp::auth::PlainPasswordHasher;
use rshogi_csa_server_tcp::broadcaster::InMemoryBroadcaster;
use rshogi_csa_server_tcp::rate_limit::IpLoginRateLimiter;
use rshogi_csa_server_tcp::server::{
    InMemoryPasswordStore, PasswordStore, ServerConfig, SharedState, build_state, prepare_runtime,
    run_server,
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
    /// プレイヤ定義ファイル（TOML 形式、keys = handle）。`password` フィールドを
    /// 読むのは常に本ファイル。`--players-yaml` を併用するとレート関連フィールド
    /// (`rate` / `wins` / `losses`) は YAML 側で永続管理される（TOML 側の
    /// `rate` / `wins` / `losses` は YAML 未登録プレイヤの初期値補填にのみ使う）。
    #[arg(long)]
    players: PathBuf,
    /// Ruby shogi-server 互換の `players.yaml` パス。指定すると終局時に
    /// 勝敗・最終対局 ID・最終更新時刻が atomic write で書き戻される。
    /// 未指定時はインメモリ保存（再起動で wins/losses が失われる開発用）。
    /// `--allow-floodgate-features` の opt-in が立っていない場合は起動が失敗する。
    #[arg(long, value_name = "PATH")]
    players_yaml: Option<PathBuf>,
    /// Floodgate スケジュール宣言の TOML パス。`[[schedules]]` 配列で複数の
    /// スケジュールを指定できる（`game_name` / `weekday` / `hour` / `minute` /
    /// `clock` / `pairing_strategy`）。指定すると定刻起動マッチメイクが有効化
    /// され、`--allow-floodgate-features` opt-in が必須になる。
    #[arg(long, value_name = "PATH")]
    floodgate_schedule_toml: Option<PathBuf>,
    /// Floodgate 履歴 JSONL ファイルのパス。指定すると終局時に 1 entry / 1 line
    /// で append される（開始時刻・ペア・結果・勝者）。`--allow-floodgate-features`
    /// opt-in が必須。
    #[arg(long, value_name = "PATH")]
    floodgate_history_jsonl: Option<PathBuf>,
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
    /// Prometheus 互換メトリクスを expose する HTTP listener の bind 先（例:
    /// `127.0.0.1:9090`）。未指定時はメトリクス recorder を install せず、
    /// `metrics::counter!` 等は NoOp として実行される（軽量な atomic 1 回程度の
    /// オーバーヘッド）。Prometheus の scrape 先として運用する場合のみ指定する。
    #[arg(long)]
    metrics_bind: Option<String>,
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

    // `--metrics-bind` 指定時のみ Prometheus exporter を install する。未指定時は
    // recorder 未 install のまま、`metrics::counter!` 等は NoOp で動く。exporter
    // は別 thread で multi-threaded Tokio runtime を立てて HTTP listener を持ち、
    // 本クレートの `current_thread` + `LocalSet` 設計とは独立して動作する。
    if let Some(raw) = cli.metrics_bind.as_deref() {
        let metrics_addr: std::net::SocketAddr =
            raw.parse().with_context(|| format!("bad --metrics-bind {raw}"))?;
        rshogi_csa_server_tcp::metrics::init_prometheus_exporter(metrics_addr)
            .with_context(|| format!("install Prometheus exporter on {metrics_addr}"))?;
        tracing::info!(bind = %metrics_addr, "Prometheus metrics exporter ready");
        // `/metrics` は plain HTTP で auth も無いため、非 loopback bind は
        // 公開ネットへ漏らす事故になり得る。reverse proxy (nginx/envoy) で
        // basic auth / TLS / IP 制限をかける運用前提だが、その手前で誤って
        // `0.0.0.0:9090` 等を直接公開していないかを起動時に警告する。
        if !metrics_addr.ip().is_loopback() {
            tracing::warn!(
                bind = %metrics_addr,
                "metrics endpoint is bound to a non-loopback address; \
                 ensure a reverse proxy enforces auth / TLS / IP allowlist before exposing it"
            );
        }
    }

    // 1. プレイヤ定義ファイルを読む。TOML の `[players.<handle>]` エントリで表現する。
    let (password_map, rate_map) = load_players_toml(&cli.players)
        .with_context(|| format!("failed to load players file {:?}", cli.players))?;
    let password_store = InMemoryPasswordStore { map: password_map };

    // Floodgate スケジュール TOML を読み込む（指定時のみ）。
    let floodgate_schedules = if let Some(path) = cli.floodgate_schedule_toml.as_ref() {
        load_floodgate_schedule_toml(path)
            .with_context(|| format!("failed to load floodgate schedule TOML at {path:?}"))?
    } else {
        Vec::new()
    };

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
        // `cli.players_yaml` を一旦クローンして config に乗せる。下の `async move`
        // ブロックが分岐ごとに `cli.players_yaml` の所有権を取れるよう、ここでは
        // 1 回だけ clone する（残りはそのまま move される）。
        players_yaml_path: cli.players_yaml.clone(),
        floodgate_schedules,
        floodgate_history_path: cli.floodgate_history_jsonl.clone(),
        shutdown_grace: std::time::Duration::from_secs(cli.shutdown_grace_sec),
    };
    // Floodgate 系機能の opt-in ゲートを起動前に評価する。`players_yaml_path` が
    // `Some` の状態は `enable_persistent_player_rates` 要求として intent に乗るため、
    // `--allow-floodgate-features` が立っていなければここで fail-fast する。
    prepare_runtime(&config).map_err(|msg| {
        anyhow::anyhow!(
            "{msg}; pass {ALLOW_FLOODGATE_FEATURES_FLAG} to enable Floodgate runtime features",
        )
    })?;

    let kifu_storage = FileKifuStorage::new(config.kifu_topdir.clone());

    // 3. port trait の `async fn in trait` は `Send` 境界を持たないため、TCP バイナリは
    //    `current_thread` ランタイム + `LocalSet` 経路で配線する（設計方針）。
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let local = tokio::task::LocalSet::new();
    // `cli.players_yaml` の所有権を async 内に move する（追加 clone を避ける）。
    let players_yaml = cli.players_yaml;
    local.block_on(&rt, async move {
        // 4. レートストレージを `--players-yaml` の有無で切り替える。
        //    YAML 経路: 起動時に既存ファイルを読み、YAML 未登録の handle を
        //              TOML 由来の `PlayerRateRecord`（rate / wins / losses）で
        //              in-memory 補填する。書き戻しは `record_game_outcome` 経由
        //              で atomic に行う。
        //    None 経路: TOML から再構築するインメモリ保存。再起動で wins/losses が
        //              失われるが、開発・テスト用途には十分。
        if let Some(yaml_path) = players_yaml {
            // load_from_file が PathBuf を消費するので、エラーメッセージ用には
            // path 文字列を先に確保する（追加 PathBuf clone を避ける）。
            let path_for_err = yaml_path.display().to_string();
            let storage = PlayersYamlRateStorage::load_from_file(yaml_path)
                .await
                .with_context(|| format!("failed to load players.yaml at {path_for_err}"))?;
            // TOML 由来の handle でまだ YAML 上に存在しないものを TOML 値そのままで
            // 補填する。YAML 既存レコード側は ensure_default_records が保護する。
            // `into_values()` で `PlayerRateRecord` 全体を渡すことで、TOML の
            // rate / wins / losses が初期値補填経路でデータ破壊なく反映される。
            storage.ensure_default_records(rate_map.into_values());
            run_with_state(config, storage, kifu_storage, password_store).await
        } else {
            let storage = InMemoryRateStorage::new(rate_map);
            run_with_state(config, storage, kifu_storage, password_store).await
        }
    })?;
    Ok(())
}

/// 構築済みの依存（rate / kifu / password）から `SharedState` を組み立てて
/// accept ループを起動し、SIGINT / SIGTERM 受信後の graceful shutdown を完遂する
/// 共通経路。`R` を YAML / インメモリで切り替えるための monomorphize 用ヘルパ。
async fn run_with_state<R, K, P>(
    config: ServerConfig,
    rate_storage: R,
    kifu_storage: K,
    password_store: P,
) -> anyhow::Result<()>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let state: Rc<SharedState<R, K, P>> = Rc::new(build_state(
        config,
        rate_storage,
        kifu_storage,
        password_store,
        Box::new(PlainPasswordHasher::new()),
        IpLoginRateLimiter::default_limits(),
        InMemoryBroadcaster::new(),
    ));

    let handle = run_server(state.clone()).await.context("run_server")?;
    tracing::info!("rshogi-csa-server-tcp ready");

    // Floodgate スケジューラを起動。`floodgate_schedules` が空なら無動作。
    // `run_schedules` は各スケジュールを `spawn_local` で独立タスク化して
    // JoinHandle を返す。各タスクは内部で `state.shutdown` を監視して
    // shutdown 時に自動終了する。
    let scheduler_handles = rshogi_csa_server_tcp::scheduler::run_schedules(state.clone())
        .map_err(|msg| anyhow::anyhow!("failed to start floodgate scheduler: {msg}"))?;
    if !scheduler_handles.is_empty() {
        tracing::info!(schedules = scheduler_handles.len(), "floodgate scheduler tasks started");
    }

    // SIGINT と SIGTERM を並列待機する。SIGINT は Ctrl-C、SIGTERM は
    // systemd / Docker / Kubernetes の停止シグナル。
    let sig = wait_for_termination_signal().await;
    tracing::info!(signal = sig, "initiating graceful shutdown");

    // 1. 新規接続の受付停止 + 待機プール中のセッション切断を誘導する。
    state.shutdown.trigger();

    // 2. accept ループの終了を待つ。shutdown が立っているので即座に抜ける。
    //    panic した場合に listener 未解放のまま後段に進まないよう、panic は
    //    error log に落として正常経路と同様に fall-through させる。
    match handle.await {
        Ok(()) => {}
        Err(e) if e.is_panic() => {
            tracing::error!(error = %e, "accept loop panicked during shutdown");
        }
        Err(e) => {
            tracing::info!(error = %e, "accept loop joined with error");
        }
    }

    // 2.5 scheduler tasks の完了も待つ。`state.shutdown` が立っているので各
    //     スケジューラループは select! で抜けて自然終了する。残りの対局完了は
    //     後続の active_drive_tasks 待機で吸収する。
    for h in scheduler_handles {
        let _ = h.await;
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
/// `EnvFilter::new("info")` により target 指定なしの `info` 全体フィルタを
/// 適用する。`tracing-log` ブリッジ (`LogTracer::init`) を明示的に起動するため、
/// 依存先 crate が `log::info!` 等の `log` macro を使っていても tracing
/// subscriber に流れて 1 系統の出力になる。
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
    // なるため常時 off。色付きログを欲しい運用要件が出てきたら
    // `IsTerminal` 自動判定なり env toggle なりをその時点で導入する（YAGNI）。
    // `tracing-subscriber` の `tracing-log` feature だけでは依存先 crate の
    // `log::*` macro 出力は subscriber に流れない。`LogTracer::init()` を
    // subscriber 初期化前に呼んで log -> tracing ブリッジを明示的に起動する。
    let _ = tracing_log::LogTracer::init();
    let registry = tracing_subscriber::registry().with(filter).with(fmt::layer().with_ansi(false));
    // 多重 init はテスト harness 等で発生し得るため失敗を許容する。
    let _ = registry.try_init();
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

/// Floodgate スケジュール TOML を読む。
///
/// 期待する形式:
/// ```toml
/// [[schedules]]
/// game_name = "floodgate-600-10"
/// weekday = "Mon"
/// hour = 13
/// minute = 0
/// pairing_strategy = "direct"
///
/// [schedules.clock]
/// kind = "countdown"
/// total_time_sec = 600
/// byoyomi_sec = 10
/// ```
fn load_floodgate_schedule_toml(
    path: &std::path::Path,
) -> anyhow::Result<Vec<rshogi_csa_server::FloodgateSchedule>> {
    use serde::Deserialize;
    #[derive(Debug, Deserialize)]
    struct Root {
        #[serde(default)]
        schedules: Vec<rshogi_csa_server::FloodgateSchedule>,
    }
    let raw = std::fs::read_to_string(path)?;
    let root: Root = toml::from_str(&raw)?;
    Ok(root.schedules)
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
