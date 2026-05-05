//! CSA対局クライアント
//!
//! USIエンジンをCSAプロトコル対局サーバー（floodgate等）に接続し、
//! CLIからバックグラウンドで連続対局を実行する。
//!
//! # 使用例
//!
//! ```bash
//! # TOML設定ファイルから実行
//! cargo run -p rshogi-csa-client -- config.toml
//!
//! # CLIオプションでオーバーライド
//! cargo run -p rshogi-csa-client -- config.toml --id my_engine --ponder
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};

use rshogi_csa_client::config::CsaClientConfig;
use rshogi_csa_client::engine::UsiEngine;
use rshogi_csa_client::events::SessionOutcome;
use rshogi_csa_client::jsonl::write_game_jsonl;
use rshogi_csa_client::protocol::{CsaConnection, GameResult};
use rshogi_csa_client::record::save_record;
use rshogi_csa_client::session::{run_game_session, run_resumed_session};
use rshogi_csa_client::transport::{ConnectOpts, TransportTarget};

/// `--target` プリセット。本リポ単一 Cloudflare アカウント (`sh11235.workers.dev`) の
/// staging / production Worker への 1 コマンド接続を提供する。別アカウントの Worker に
/// 接続する場合は `--target` を使わず TOML / `--host` で URL を直接指定する。
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum TargetPreset {
    /// staging Worker (`rshogi-csa-server-workers-staging.sh11235.workers.dev`)。
    /// `WS_ALLOWED_ORIGINS = "https://csa-client-local"` allowlist 通過のため
    /// `Origin: https://csa-client-local` を送る。
    Staging,
    /// production Worker (`rshogi-csa-server-workers.sh11235.workers.dev`)。
    /// `WS_ALLOWED_ORIGINS = ""` (空) の運用前提でネイティブ経路として Origin を
    /// 送らない (=`ws_origin = None`)。
    Production,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CliColor {
    Black,
    White,
}

impl CliColor {
    fn as_str(self) -> &'static str {
        match self {
            CliColor::Black => "black",
            CliColor::White => "white",
        }
    }
}

#[derive(Parser)]
#[command(
    name = "csa_client",
    about = "CSA対局クライアント — USIエンジンをCSAサーバーに接続"
)]
struct Cli {
    /// TOML設定ファイルのパス
    config: Option<PathBuf>,

    /// 接続先プリセット。`--target {staging,production}` で本リポ単一アカウントの
    /// Worker に 1 コマンドで繋がる。`--room-id` / `--handle` / `--color` を併指定する。
    #[arg(long, value_enum)]
    target: Option<TargetPreset>,

    /// `--target` 利用時の room_id。Worker は `/ws/<room_id>` でルームを区切る。
    /// 黒・白で同じ値を入れること（マッチング成立条件）。
    #[arg(long)]
    room_id: Option<String>,

    /// `--target` 利用時のログインハンドル。CSA LOGIN ID は
    /// `<handle>+<game_name>+<color>` 形式で組み立てる（Workers GameRoom 要求）。
    /// `<game_name>` は `--game-name` の値、未指定時は `<room_id>` で fallback。
    #[arg(long)]
    handle: Option<String>,

    /// `--target` 利用時の手番。
    #[arg(long, value_enum)]
    color: Option<CliColor>,

    /// LobbyDO マッチングモード。`--target` と `--handle` / `--color` 併用必須。
    /// `/ws/lobby` に接続して LOGIN_LOBBY → MATCHED 受信 → 指定された room_id へ
    /// 接続して対局するループを `--max-games` まで繰り返す。
    #[arg(long, default_value_t = false)]
    lobby: bool,

    /// LOGIN handle の `<game_name>` 部分。`--target` 利用時、worker の `CLOCK_PRESETS`
    /// に登録された preset 名 (例: `floodgate-600-10`) を指定して clock を選ぶ。
    /// `--lobby` 利用時はマッチング pool 名としても用いる (同 game_name 同士でしか
    /// マッチングしない)。`[A-Za-z0-9_-]` / 1〜32 文字制限。
    /// 非 lobby `--target` 経路で省略した場合、後方互換のため `<room_id>` を
    /// `<game_name>` として使う (`CLOCK_PRESETS` 未登録の Worker 向け既定動作)。
    #[arg(long)]
    game_name: Option<String>,

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

    /// WebSocket Upgrade 時の Origin ヘッダ値（`wss://` 接続時のみ意味あり）。
    /// 例: `https://csa-client.example.local`
    #[arg(long)]
    ws_origin: Option<String>,

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

    /// JSONL 出力ディレクトリ。指定すると対局完了ごとに analyze_selfplay 互換の
    /// JSONL ファイルを `<datetime>_<sente>_vs_<gote>.jsonl` として書き出す。
    /// 未指定時は JSONL を出力しない（既定 OFF）。TOML の `record.jsonl_out` でも指定可。
    #[arg(long)]
    jsonl_out: Option<PathBuf>,

    /// USIエンジンオプション (K=V,K=V,...)
    #[arg(long)]
    options: Option<String>,
}

fn main() -> Result<()> {
    // tungstenite が wss を扱う際、rustls 0.23 は process-level の CryptoProvider が
    // 明示的に登録されていないと panic する。aws-lc-rs より導入が軽い `ring` を選択し、
    // 起動時に 1 度だけ install する。`set_default` は同 process で既に install されて
    // いれば `Err` を返すので無視する。
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();

    // 設定ファイル読み込み
    let mut config = if let Some(ref path) = cli.config {
        CsaClientConfig::from_file(path)
            .with_context(|| format!("設定ファイル読み込み失敗: {}", path.display()))?
    } else {
        CsaClientConfig::default()
    };

    // `--target` プリセットを TOML の上に重ねる。CLI で room_id / handle / color が
    // 揃っていれば host / id / ws_origin / floodgate を 1 引数で組み立てる。
    apply_target_preset(&mut config, &cli)?;

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

        // `--lobby` モードは対局直前に LobbyDO へ問い合わせて room_id を取得する。
        let lobby_room_assignment = if cli.lobby {
            match acquire_lobby_match(&config, &cli, &shutdown) {
                Ok(Some(assignment)) => Some(assignment),
                Ok(None) => break, // shutdown
                Err(e) => {
                    log::error!("ロビー接続エラー: {e}");
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    log::info!("{}秒後にリトライ...", retry_delay.as_secs());
                    std::thread::sleep(retry_delay);
                    retry_delay =
                        (retry_delay * 2).min(Duration::from_secs(config.retry.max_delay_sec));
                    continue;
                }
            }
        } else {
            None
        };
        let game_config = if let Some(ref assignment) = lobby_room_assignment {
            config_with_lobby_assignment(&config, assignment)
        } else {
            config.clone()
        };

        match run_one_game(&game_config, &mut engine, &shutdown, games_played) {
            Ok((result, record)) => {
                // 棋譜保存
                if let Err(e) = save_record(&record, &config.record) {
                    log::error!("棋譜保存エラー: {e}");
                }

                // JSONL 出力（opt-in、--jsonl-out / record.jsonl_out 指定時のみ）
                if let Some(ref jsonl_dir) = config.record.jsonl_out {
                    match write_game_jsonl(jsonl_dir, &record, &config, &result) {
                        Ok(path) => log::info!("[REC] JSONL 保存: {}", path.display()),
                        Err(e) => log::error!("JSONL 保存エラー: {e}"),
                    }
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

/// 1回のゲームを実行する（接続〜対局〜切断）。
///
/// `games_played` は本起動セッションでの対局完了数（0 開始）。`config.server.host` /
/// `config.server.id` に `{game_seq}` placeholder が含まれていれば
/// `games_played` に置換する。Cloudflare Workers サーバは 1 DO instance =
/// 1 対局という設計のため、連続対局では room_id を毎局変える必要があり、
/// この placeholder で host URL を局ごとに分岐する運用を提供する。本家 Floodgate の
/// ように同 server を多対局に使う場合は placeholder を入れず host を固定すればよい。
fn run_one_game(
    config: &CsaClientConfig,
    engine: &mut UsiEngine,
    shutdown: &AtomicBool,
    games_played: u32,
) -> Result<(GameResult, rshogi_csa_client::record::GameRecord)> {
    let game_seq_str = games_played.to_string();
    let host = config.server.host.replace("{game_seq}", &game_seq_str);
    let id = config.server.id.replace("{game_seq}", &game_seq_str);

    // サーバー接続。host に scheme (`ws://` / `wss://` / `tcp://`) があれば
    // それに従い、無ければ既存挙動どおり `host:port` の TCP。
    let target = TransportTarget::from_host_port(&host, config.server.port)?;
    let opts = ConnectOpts {
        tcp_keepalive: config.server.keepalive.tcp,
        ws_origin: config.server.ws_origin.clone(),
    };
    let mut conn = CsaConnection::connect_with_target(&target, &opts)?;
    conn.login(&id, &config.server.password)?;

    // 対局実行
    let session_result = run_game_session(&mut conn, engine, config, shutdown);

    // 中断 (= サーバ切断) でかつ Reconnect_Token を保持している場合は 1 度だけ
    // 再接続を試みる。Workers の `RECONNECT_GRACE_SECONDS` 内に到達できれば
    // 同一対局を継続できる。reconnect 試行前に元 conn は drop / logout する。
    //
    // 判定ロジック自体は `should_attempt_reconnect` pure helper に切り出して
    // unit test で pin している。token=None (production の grace=0 構成等) の
    // 場合は reconnect 試行自体を skip するため、Issue #591 の
    // `LOGIN:incorrect reconnect_rejected` 経路に到達しない。
    let reconnect_request = match session_result.as_ref() {
        Ok(outcome) => should_attempt_reconnect(outcome, shutdown.load(Ordering::SeqCst)),
        Err(_) => None,
    };

    if let Some((game_id, token)) = reconnect_request {
        log::warn!(
            "[CSA] サーバ切断を検出。Reconnect_Token を持つので grace 内に再接続を試みます: game_id={}",
            game_id
        );
        let _ = conn.logout();
        drop(conn);
        let credentials = ReconnectCredentials {
            id: &id,
            password: &config.server.password,
            game_id: &game_id,
            token: &token,
        };
        match attempt_reconnect(&target, &opts, &credentials, engine, config, shutdown) {
            Ok((reconnect_result, reconnect_record)) => {
                log::info!("[CSA] 再接続成功: 対局を継続して終局: {:?}", reconnect_result);
                return Ok((reconnect_result, reconnect_record));
            }
            Err(e) => {
                // engine は元 disconnect 経路で `gameover("lose")` 発射済み。
                // `attempt_reconnect` 中に `engine.new_game()` が成功した後で失敗
                // するケースではエンジンが「新局面待ち」状態のまま残るが、
                // `stop_and_wait()` は探索中でない場合は no-op として通過する
                // ことを期待する (rshogi-usi 含む主要 USI engine の挙動)。
                log::warn!("[CSA] 再接続失敗: {e}。元の Interrupted 結果で終了します。");
                let _ = engine.stop_and_wait();
                return session_result
                    .map(|outcome| (outcome.result, outcome.record))
                    .map_err(|e| anyhow::anyhow!("{}", e));
            }
        }
    }

    // エラー時は投了を試みる（NF2: 対局中のエラーは投了してから再接続）
    if session_result.is_err() {
        // ponder 中の場合は stop して bestmove を待ってからクリーンアップ
        let _ = engine.stop_and_wait();
        let _ = conn.send_resign();
        let _ = engine.gameover("lose");
    }

    let _ = conn.logout();
    session_result
        .map(|outcome| (outcome.result, outcome.record))
        .map_err(|e| anyhow::anyhow!("{}", e))
}

/// セッション結果から再接続を試みるべきかどうかを判定する pure helper。
///
/// 戻り値の `(game_id, token)` 順は [`ReconnectCredentials`] の `game_id` /
/// `token` フィールド順と一致させる。順序を逆転させると `attempt_reconnect`
/// が不正な認証情報を server に送るので unit test で pin する。
///
/// 戻り値:
/// - `Some((game_id, token))`: reconnect を試みるべき (中断 + token あり + 非 shutdown)
/// - `None`: reconnect を skip すべき (shutdown 中 / 通常終局 / token 未配布)
///
/// production の grace=0 構成では server から `Reconnect_Token:` 拡張行が出ないため
/// `outcome.summary.reconnect_token` が常に `None` で、本関数は `None` を返し、
/// Issue #591 の `LOGIN:incorrect reconnect_rejected` 経路には到達しない。
fn should_attempt_reconnect(outcome: &SessionOutcome, shutdown: bool) -> Option<(String, String)> {
    if shutdown || outcome.result != GameResult::Interrupted {
        return None;
    }
    let summary = outcome.summary.as_ref()?;
    let token = summary.reconnect_token.as_ref()?.clone();
    Some((summary.game_id.clone(), token))
}

/// 切断検出後の再接続に必要な認証情報。多引数化を避けるためまとめる。
struct ReconnectCredentials<'a> {
    id: &'a str,
    password: &'a str,
    game_id: &'a str,
    token: &'a str,
}

/// 切断検出後の自動再接続。新規 transport で接続 → `LOGIN ... reconnect:<game_id>+<token>`
/// → resume 用 Game_Summary + Reconnect_State 受信 → セッションループ継続。
fn attempt_reconnect(
    target: &TransportTarget,
    opts: &ConnectOpts,
    creds: &ReconnectCredentials<'_>,
    engine: &mut UsiEngine,
    config: &CsaClientConfig,
    shutdown: &AtomicBool,
) -> Result<(GameResult, rshogi_csa_client::record::GameRecord)> {
    let mut conn = CsaConnection::connect_with_target(target, opts)?;
    conn.login_reconnect(creds.id, creds.password, creds.game_id, creds.token)?;
    let outcome = run_resumed_session(&mut conn, engine, config, shutdown)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let _ = conn.logout();
    Ok((outcome.result, outcome.record))
}

/// `--lobby` モードで成立したマッチ。`run_one_game` 直前に config を差し替えるための
/// host / id 値を保持する。
struct LobbyAssignment {
    /// MATCHED で受け取った WS URL (`wss://<subdomain>/ws/<room_id>`)。
    host: String,
    /// `<handle>+<game_name>+<color>` 形式の LOGIN ID (GameRoom DO 側のフォーマット)。
    id: String,
}

/// LobbyDO に接続して 1 ペア分のマッチング結果を取得する。
///
/// shutdown 信号が立ったら `Ok(None)` を返す (呼び出し側ループを抜ける)。
fn acquire_lobby_match(
    config: &CsaClientConfig,
    cli: &Cli,
    shutdown: &AtomicBool,
) -> Result<Option<LobbyAssignment>> {
    let game_name = cli
        .game_name
        .as_deref()
        .ok_or_else(|| anyhow!("--lobby を指定する場合 --game-name <name> も指定してください"))?;
    let handle = cli
        .handle
        .as_deref()
        .ok_or_else(|| anyhow!("--lobby を指定する場合 --handle <name> も指定してください"))?;
    let color = cli.color.ok_or_else(|| {
        anyhow!("--lobby を指定する場合 --color <black|white> も指定してください")
    })?;

    // config.server.host は `apply_target_preset` で `wss://<subdomain>/ws/lobby` に
    // セットされている前提。`/ws/lobby` 以外の経路 (例: TOML 直書きの `wss://.../ws/<room>`)
    // は LobbyDO に到達しないため early reject する。
    if !config.server.host.ends_with("/ws/lobby") {
        bail!(
            "--lobby は --target staging|production 経由で設定する想定です (host={})",
            config.server.host
        );
    }

    let target = TransportTarget::from_host_port(&config.server.host, config.server.port)?;
    let opts = ConnectOpts {
        tcp_keepalive: config.server.keepalive.tcp,
        ws_origin: config.server.ws_origin.clone(),
    };
    let mut conn = CsaConnection::connect_with_target(&target, &opts)?;

    let color_str = match color {
        CliColor::Black => "black",
        CliColor::White => "white",
    };
    let login_id = format!("{handle}+{game_name}+{color_str}");
    let password = if config.server.password.is_empty() {
        PRESET_FALLBACK_PASSWORD
    } else {
        config.server.password.as_str()
    };
    let login_line = format!("LOGIN_LOBBY {login_id} {password}");
    conn.send_raw_line(&login_line)?;

    log::info!("[Lobby] LOGIN_LOBBY 送信: handle={handle} game_name={game_name} color={color_str}");

    // LOGIN_LOBBY:OK → MATCHED <room_id> <color> の順で受信。途中 shutdown が立ったら
    // logout して None を返す。
    loop {
        if shutdown.load(Ordering::SeqCst) {
            let _ = conn.send_raw_line("LOGOUT_LOBBY");
            return Ok(None);
        }
        let line = match conn.recv_line_blocking_pub(Duration::from_secs(60)) {
            Ok(l) => l,
            Err(e) => {
                bail!("[Lobby] 受信エラー: {e}");
            }
        };
        if let Some(rest) = line.strip_prefix("LOGIN_LOBBY:") {
            if rest.ends_with(" OK") {
                log::info!("[Lobby] LOGIN_LOBBY OK ({rest})、MATCHED 待機");
                continue;
            }
            if rest.starts_with("incorrect") {
                bail!("[Lobby] LOGIN_LOBBY 拒否: {rest}");
            }
            if rest == "expired" {
                bail!("[Lobby] queue TTL 超過 (LOGIN_LOBBY:expired)、再試行してください");
            }
        }
        if let Some(rest) = line.strip_prefix("MATCHED ") {
            // フォーマット: `MATCHED <room_id> <color>`
            let mut parts = rest.split_whitespace();
            let room_id = parts.next().unwrap_or("");
            let assigned_color = parts.next().unwrap_or("");
            if room_id.is_empty() || assigned_color.is_empty() {
                bail!("[Lobby] MATCHED 行のフォーマット不正: {line}");
            }
            // assigned_color は client 要望と一致するはず (DirectMatch は preferred 維持)。
            // 念のため検証して mismatch なら bail (将来 Random pairing 等に備えて)。
            if assigned_color != color_str {
                bail!(
                    "[Lobby] MATCHED の color が要望と一致しません: requested={color_str} got={assigned_color}"
                );
            }
            // host を `wss://<subdomain>/ws/<room_id>` に書き換える。
            let new_host = config
                .server
                .host
                .strip_suffix("/ws/lobby")
                .map(|prefix| format!("{prefix}/ws/{room_id}"))
                .ok_or_else(|| anyhow!("config.server.host から /ws/lobby を切り出せません"))?;
            let new_id = format!("{handle}+{game_name}+{color_str}");
            log::info!("[Lobby] MATCHED 受信: room_id={room_id} → host={new_host}");
            // LobbyDO 側は MATCHED 後に WS を close するので追加 logout は不要。
            return Ok(Some(LobbyAssignment {
                host: new_host,
                id: new_id,
            }));
        }
        log::debug!("[Lobby] 未知の line: {line}");
    }
}

/// MATCHED 受信後の host / id を反映した一時 config を返す。
fn config_with_lobby_assignment(
    base: &CsaClientConfig,
    assignment: &LobbyAssignment,
) -> CsaClientConfig {
    let mut new_config = base.clone();
    new_config.server.host = assignment.host.clone();
    new_config.server.id = assignment.id.clone();
    new_config
}

/// `--target` プリセット適用時に password が空のときに埋める placeholder 値。
/// Workers GameRoom 側は LOGIN password の値を検証しないため、空でなければ任意で
/// よい。TOML / `--password` で明示的に上書きできる。
const PRESET_FALLBACK_PASSWORD: &str = "anything";

/// `--target` プリセット適用。`--target` が未指定なら no-op。
///
/// 指定時は `--handle` / `--color` を併指定必須とし、`--lobby` 未指定時は
/// `--room-id` も必須。`--lobby` 指定時は room_id に `"lobby"` を仮置きして
/// `wss://<subdomain>/ws/lobby` URL を組み立てる (実際の room_id は MATCHED 受信後に
/// `acquire_lobby_match` で差し替える)。
fn apply_target_preset(config: &mut CsaClientConfig, cli: &Cli) -> Result<()> {
    let Some(target) = cli.target else {
        return Ok(());
    };
    let room_id_owned: String;
    let room_id: &str = if cli.lobby {
        "lobby"
    } else {
        room_id_owned = cli.room_id.clone().ok_or_else(|| {
            anyhow!("--target を指定する場合 --room-id <room_id> も指定してください")
        })?;
        &room_id_owned
    };
    let handle = cli
        .handle
        .as_deref()
        .ok_or_else(|| anyhow!("--target を指定する場合 --handle <name> も指定してください"))?;
    let color = cli.color.ok_or_else(|| {
        anyhow!("--target を指定する場合 --color <black|white> も指定してください")
    })?;

    let (subdomain, ws_origin) = match target {
        TargetPreset::Staging => (
            "rshogi-csa-server-workers-staging.sh11235.workers.dev",
            // staging Worker は `WS_ALLOWED_ORIGINS = "https://csa-client-local"` で
            // この値を allowlist に登録済み。Origin を送ってブラウザ経路相当でも
            // ネイティブ経路相当でも通電できるが、明示しておく方が allowlist 経路の
            // 動作確認に資する。
            Some("https://csa-client-local".to_owned()),
        ),
        TargetPreset::Production => (
            "rshogi-csa-server-workers.sh11235.workers.dev",
            // production Worker の `WS_ALLOWED_ORIGINS` は空 (空 allowlist 運用)。
            // Origin 欠落 = ネイティブ経路として素通し、Origin 付きは 403 になる。
            // web client 化したいときは wrangler.production.toml の allowlist に値を追加
            // してから CLI で `--ws-origin <url>` を渡す運用に切り替える。
            None,
        ),
    };
    config.server.host = format!("wss://{subdomain}/ws/{room_id}");
    // `wss://` 経路では port 値は無視されるが、TOML schema 互換のため 0 を入れておく。
    config.server.port = 0;
    // LOGIN id の `<game_name>` 部分は CLI で `--game-name` を渡されたらそれを使う
    // (worker の `CLOCK_PRESETS` 登録 preset 名と一致させて strict mode を通す)。
    // 省略時は room_id を game_name として使う後方互換動作にとどめる
    // (`CLOCK_PRESETS = "[]"` の Worker や lobby 経路の room_id == game_name 慣習向け)。
    let game_name_for_id = cli.game_name.as_deref().filter(|g| !g.is_empty()).unwrap_or(room_id);
    config.server.id = format!("{handle}+{game_name_for_id}+{}", color.as_str());
    if config.server.password.is_empty() {
        config.server.password = PRESET_FALLBACK_PASSWORD.to_owned();
    }
    // Workers GameRoom は floodgate=true 前提の挙動（Game_Summary で Floodgate
    // 拡張を返す等）なので、preset では強制 true。TOML で `floodgate = false` を
    // 入れていても上書きされる。CLI `--floodgate=false` は後段で勝つので回避可能。
    config.server.floodgate = true;
    config.server.ws_origin = ws_origin;
    Ok(())
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
    if let Some(ref origin) = cli.ws_origin {
        config.server.ws_origin = Some(origin.clone());
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
    if let Some(ref dir) = cli.jsonl_out {
        config.record.jsonl_out = Some(dir.clone());
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

    // ログファイル（設定されていれば）— 日付ファイル名で日次ローテーション
    let log_dir = config.log.dir.clone();
    let log_file = if !log_dir.as_os_str().is_empty() {
        let _ = std::fs::create_dir_all(&log_dir);
        let date = chrono::Local::now().format("%Y-%m-%d");
        let path = log_dir.join(format!("csa_client_{date}.log"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_with(target: Option<TargetPreset>) -> Cli {
        Cli {
            config: None,
            target,
            room_id: None,
            handle: None,
            color: None,
            lobby: false,
            game_name: None,
            host: None,
            port: None,
            id: None,
            password: None,
            engine: None,
            hash: None,
            ponder: None,
            floodgate: None,
            ws_origin: None,
            keep_alive: None,
            margin_msec: None,
            max_games: None,
            log_level: None,
            record_dir: None,
            jsonl_out: None,
            options: None,
        }
    }

    #[test]
    fn target_preset_no_op_when_target_unset() {
        let mut config = CsaClientConfig::default();
        let original_host = config.server.host.clone();
        apply_target_preset(&mut config, &cli_with(None)).unwrap();
        assert_eq!(config.server.host, original_host);
        assert!(config.server.id.is_empty());
    }

    #[test]
    fn target_preset_requires_room_id() {
        let mut config = CsaClientConfig::default();
        let cli = cli_with(Some(TargetPreset::Staging));
        let err = apply_target_preset(&mut config, &cli).unwrap_err();
        assert!(err.to_string().contains("--room-id"));
    }

    #[test]
    fn target_preset_requires_handle() {
        let mut config = CsaClientConfig::default();
        let mut cli = cli_with(Some(TargetPreset::Staging));
        cli.room_id = Some("r".to_owned());
        let err = apply_target_preset(&mut config, &cli).unwrap_err();
        assert!(err.to_string().contains("--handle"));
    }

    #[test]
    fn target_preset_requires_color() {
        let mut config = CsaClientConfig::default();
        let mut cli = cli_with(Some(TargetPreset::Staging));
        cli.room_id = Some("r".to_owned());
        cli.handle = Some("h".to_owned());
        let err = apply_target_preset(&mut config, &cli).unwrap_err();
        assert!(err.to_string().contains("--color"));
    }

    #[test]
    fn target_preset_staging_fills_host_and_id() {
        let mut config = CsaClientConfig::default();
        let mut cli = cli_with(Some(TargetPreset::Staging));
        cli.room_id = Some("e2e-1".to_owned());
        cli.handle = Some("alice".to_owned());
        cli.color = Some(CliColor::Black);
        apply_target_preset(&mut config, &cli).unwrap();
        assert_eq!(
            config.server.host,
            "wss://rshogi-csa-server-workers-staging.sh11235.workers.dev/ws/e2e-1"
        );
        assert_eq!(config.server.id, "alice+e2e-1+black");
        assert_eq!(config.server.ws_origin.as_deref(), Some("https://csa-client-local"));
        assert!(!config.server.password.is_empty());
        assert!(config.server.floodgate);
    }

    #[test]
    fn target_preset_production_omits_origin() {
        let mut config = CsaClientConfig::default();
        let mut cli = cli_with(Some(TargetPreset::Production));
        cli.room_id = Some("game42".to_owned());
        cli.handle = Some("bob".to_owned());
        cli.color = Some(CliColor::White);
        apply_target_preset(&mut config, &cli).unwrap();
        assert_eq!(
            config.server.host,
            "wss://rshogi-csa-server-workers.sh11235.workers.dev/ws/game42"
        );
        assert_eq!(config.server.id, "bob+game42+white");
        assert!(config.server.ws_origin.is_none());
    }

    #[test]
    fn target_preset_uses_game_name_in_login_id() {
        // `--target` 非 lobby モードで `--game-name <preset>` を渡すと、URL の
        // `<room_id>` と LOGIN id の `<game_name>` を独立に組み立てる
        // (`CLOCK_PRESETS` strict mode で preset 名 LOGIN を成立させるため)。
        let mut config = CsaClientConfig::default();
        let mut cli = cli_with(Some(TargetPreset::Staging));
        cli.room_id = Some("e2e-room-1".to_owned());
        cli.handle = Some("alice".to_owned());
        cli.color = Some(CliColor::Black);
        cli.game_name = Some("floodgate-600-10".to_owned());
        apply_target_preset(&mut config, &cli).unwrap();
        assert_eq!(
            config.server.host,
            "wss://rshogi-csa-server-workers-staging.sh11235.workers.dev/ws/e2e-room-1"
        );
        assert_eq!(config.server.id, "alice+floodgate-600-10+black");
    }

    #[test]
    fn target_preset_falls_back_to_room_id_when_game_name_omitted() {
        // 後方互換: `--game-name` 省略時は room_id を game_name として使う
        // (`CLOCK_PRESETS = "[]"` 構成や lobby room_id == game_name 慣習向け)。
        let mut config = CsaClientConfig::default();
        let mut cli = cli_with(Some(TargetPreset::Staging));
        cli.room_id = Some("legacy-room".to_owned());
        cli.handle = Some("alice".to_owned());
        cli.color = Some(CliColor::Black);
        cli.game_name = None;
        apply_target_preset(&mut config, &cli).unwrap();
        assert_eq!(config.server.id, "alice+legacy-room+black");
    }

    #[test]
    fn target_preset_keeps_existing_password() {
        let mut config = CsaClientConfig::default();
        config.server.password = "user-supplied".to_owned();
        let mut cli = cli_with(Some(TargetPreset::Staging));
        cli.room_id = Some("r".to_owned());
        cli.handle = Some("h".to_owned());
        cli.color = Some(CliColor::Black);
        apply_target_preset(&mut config, &cli).unwrap();
        assert_eq!(config.server.password, "user-supplied");
    }

    #[test]
    fn cli_override_wins_over_target_preset() {
        // `--target staging` で host/id を埋めた後、`--host` / `--id` が CLI override
        // で最終的に勝つことを保証する（mix 利用での挙動）。
        let mut config = CsaClientConfig::default();
        let mut cli = cli_with(Some(TargetPreset::Staging));
        cli.room_id = Some("r".to_owned());
        cli.handle = Some("h".to_owned());
        cli.color = Some(CliColor::Black);
        cli.host = Some("wss://custom.example/ws/x".to_owned());
        cli.id = Some("override-id".to_owned());
        apply_target_preset(&mut config, &cli).unwrap();
        apply_cli_overrides(&mut config, &cli);
        assert_eq!(config.server.host, "wss://custom.example/ws/x");
        assert_eq!(config.server.id, "override-id");
    }

    /// `should_attempt_reconnect` の fixture を作る helper。token / shutdown / result を
    /// 引数で切り替えて全分岐を網羅する。`GameRecord` 等の重量フィールドはダミー値で OK。
    fn session_outcome_with(result: GameResult, reconnect_token: Option<String>) -> SessionOutcome {
        use rshogi_csa::{Color as CsaColor, Position};
        use rshogi_csa_client::protocol::{GameSummary, TimeConfig};
        use rshogi_csa_client::record::GameRecord;

        let summary = GameSummary {
            game_id: "room-1-test-id".to_owned(),
            my_color: CsaColor::Black,
            sente_name: "alice".to_owned(),
            gote_name: "bob".to_owned(),
            position: Position::default(),
            initial_moves: vec![],
            black_time: TimeConfig {
                total_time_ms: 60_000,
                byoyomi_ms: 1_000,
                increment_ms: 0,
            },
            white_time: TimeConfig {
                total_time_ms: 60_000,
                byoyomi_ms: 1_000,
                increment_ms: 0,
            },
            reconnect_token,
        };
        SessionOutcome {
            result,
            record: GameRecord {
                game_id: summary.game_id.clone(),
                sente_name: summary.sente_name.clone(),
                gote_name: summary.gote_name.clone(),
                black_time: summary.black_time.clone(),
                white_time: summary.white_time.clone(),
                initial_position: Position::default(),
                moves: vec![],
                result: String::new(),
                start_time: chrono::Local::now(),
                my_color: CsaColor::Black,
                jsonl_moves: vec![],
            },
            summary: Some(summary),
        }
    }

    /// `Reconnect_Token:` が配布された + 中断 + 非 shutdown なら reconnect を試みる。
    /// 戻り値の `(game_id, token)` 順を pin する (順序を逆転させると `ReconnectCredentials`
    /// の `id`/`password`/`game_id`/`token` 配線で server 側に不正な認証を送ってしまう)。
    #[test]
    fn should_attempt_reconnect_returns_some_when_token_present_and_interrupted() {
        let outcome = session_outcome_with(GameResult::Interrupted, Some("a".repeat(32)));
        let actual = should_attempt_reconnect(&outcome, false);
        let (game_id, token) = actual.expect("token あり + 中断 + 非 shutdown で Some");
        assert_eq!(game_id, "room-1-test-id");
        assert_eq!(token, "a".repeat(32));
    }

    /// production の保守的既定 (`grace=0`) で `Reconnect_Token:` 拡張行が出ない場合、
    /// `summary.reconnect_token == None` なので reconnect 試行を skip する。
    /// 本 case が Issue #591 hotfix の client 側保証 (= `LOGIN:incorrect reconnect_rejected`
    /// 経路に到達しない) を pin する唯一の test。
    #[test]
    fn should_attempt_reconnect_returns_none_when_token_absent() {
        let outcome = session_outcome_with(GameResult::Interrupted, None);
        assert!(should_attempt_reconnect(&outcome, false).is_none());
    }

    /// shutdown フラグが立っていれば、token があっても reconnect を試みない。
    /// session を畳む経路で再接続要求を送ると server に二重ログイン扱いされる
    /// 可能性があるため、shutdown 検知を最優先する契約を pin する。
    #[test]
    fn should_attempt_reconnect_returns_none_when_shutdown_set() {
        let outcome = session_outcome_with(GameResult::Interrupted, Some("a".repeat(32)));
        assert!(should_attempt_reconnect(&outcome, true).is_none());
    }

    /// 通常終局 (`Win` / `Lose` / `Draw` 等の `Interrupted` 以外) では reconnect を
    /// 試みない。`run_one_game` 側で対局が完走したことが確定しているため、
    /// 再接続経路に進むのは契約違反。
    #[test]
    fn should_attempt_reconnect_returns_none_when_result_is_not_interrupted() {
        let outcome = session_outcome_with(GameResult::Win, Some("a".repeat(32)));
        assert!(should_attempt_reconnect(&outcome, false).is_none());
    }
}
