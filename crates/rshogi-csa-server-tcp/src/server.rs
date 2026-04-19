//! TCP 受付ループと 1 接続分のセッションドライバ。
//!
//! Phase 1 MVP では以下の流れを 1 タスクで駆動する:
//!
//! 1. `TcpListener` で受理 → 1 接続を [`TcpTransport`] でラップ
//! 2. [`IpLoginRateLimiter::record`] で同一 IP からの連続 LOGIN 試行を抑制
//! 3. LOGIN 行を受理し、[`authenticate`] で RateStorage + PasswordStore を照合
//! 4. `PlayerName` を `<handle>+<game_name>+<color>` で分解し
//!    ([`parse_handle`]）、[`League`] に登録して待機プールに積む
//! 5. 相補手番の相手が到着したら、2 接続分の [`TcpTransport`] を現タスクが所有して
//!    Game_Summary 送信 → 双方の AGREE → [`run_room`] を駆動
//! 6. 終局確定で CSA V2 棋譜を保存し、00LIST に追記して両者の状態を `Finished` に遷移
//!
//! 設計上のキーポイント:
//! - 相手待ちのプレイヤは「待機スロット」として `TcpTransport` を一時所有し、
//!   次に到着したプレイヤ（drive 側）がそれを受け取って対局を駆動する。
//! - 待機スロット側のタスクは `oneshot::Receiver` で対局終了を待ち、
//!   駆動側タスクが後片付けを完了した時点で終了する。
//! - 認証失敗・LOGIN レート超過・プロトコル不正はその場でソケットを閉じる。

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use rshogi_core::types::EnteringKingRule;
use rshogi_csa_server::error::{ProtocolError, ServerError};
use rshogi_csa_server::game::clock::SecondsCountdownClock;
use rshogi_csa_server::game::result::GameResult;
use rshogi_csa_server::game::room::{GameRoom, GameRoomConfig};
use rshogi_csa_server::matching::league::{League, LoginResult, MatchedPair, PlayerStatus};
use rshogi_csa_server::port::{
    BroadcastTag, Broadcaster, ClientTransport, GameSummaryEntry, KifuStorage, RateDecision,
    RateStorage,
};
use rshogi_csa_server::protocol::command::{ClientCommand, parse_command};
use rshogi_csa_server::protocol::summary::{GameSummaryBuilder, standard_initial_position_block};
use rshogi_csa_server::record::kifu::{KifuMove, KifuRecord, primary_result_code};
use rshogi_csa_server::types::{
    Color, CsaLine, CsaMoveToken, GameId, GameName, PlayerName, RoomId,
};
use rshogi_csa_server::{FileKifuStorage, TimeClock, TransportError};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify, oneshot};
use tokio::task::JoinHandle;

use crate::auth::{AuthOutcome, PasswordHasher, authenticate};
use crate::broadcaster::InMemoryBroadcaster;
use crate::rate_limit::IpLoginRateLimiter;
use crate::transport::TcpTransport;

/// プレイヤハンドル1 件分の期待形式 (`<handle>+<game_name>+<color>`) を分解する。
///
/// color は `black` / `white` (大文字小文字は区別しない)。
/// 形式が合わなければ `None` を返し、呼び出し側は認証成功後でも LOGIN を失敗扱いにする。
pub fn parse_handle(raw: &str) -> Option<(String, GameName, Color)> {
    let mut it = raw.split('+');
    let handle = it.next()?.to_owned();
    let game_name = it.next()?.to_owned();
    let color_s = it.next()?;
    if it.next().is_some() {
        return None;
    }
    let color = match color_s.to_ascii_lowercase().as_str() {
        "black" | "b" | "sente" => Color::Black,
        "white" | "w" | "gote" => Color::White,
        _ => return None,
    };
    if handle.is_empty() || game_name.is_empty() {
        return None;
    }
    Some((handle, GameName::new(game_name), color))
}

/// 受信ループで「実質無限」として扱うタイムアウト（10 年）。
/// 実際の対局終了は持ち時間 deadline で駆動するため、`recv_line` 側は
/// この長さで貼り付けておく（`rshogi_csa_server::game::run_loop` と揃える）。
const NEAR_INFINITE: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 10);

/// サーバー起動パラメタ。
pub struct ServerConfig {
    /// bind 先。`"0.0.0.0:4081"` など。
    pub bind_addr: SocketAddr,
    /// CSA V2 棋譜と 00LIST の保存先ルート。
    pub kifu_topdir: std::path::PathBuf,
    /// Game_Summary に埋め込む持ち時間 (秒)。
    pub total_time_sec: u32,
    /// 秒読み (秒)。
    pub byoyomi_sec: u32,
    /// 通信マージン (ミリ秒)。`GameRoom` の `consume` 前に差し引かれる。
    pub time_margin_ms: u64,
    /// 最大手数。
    pub max_moves: u32,
    /// LOGIN 受信の最大待機時間。
    pub login_timeout: Duration,
    /// AGREE 受信の最大待機時間。
    ///
    /// Game_Summary 送信後、双方の AGREE / REJECT が揃うまでの受付窓。GUI
    /// クライアントや人手合意を挟む運用でも足りるよう、設定可能にしてある。
    pub agree_timeout: Duration,
    /// 入玉ルール。Phase 1 は Point24。
    pub entering_king_rule: EnteringKingRule,
}

impl ServerConfig {
    /// 動作確認用の控えめな既定値。運用では `bind_addr` と `kifu_topdir` を書き換える。
    pub fn sensible_defaults() -> Self {
        Self {
            bind_addr: "127.0.0.1:4081".parse().unwrap(),
            kifu_topdir: std::path::PathBuf::from("./kifu"),
            total_time_sec: 600,
            byoyomi_sec: 10,
            time_margin_ms: 1_500,
            max_moves: 256,
            login_timeout: Duration::from_secs(30),
            agree_timeout: Duration::from_secs(5 * 60),
            entering_king_rule: EnteringKingRule::Point24,
        }
    }
}

/// drive 側から waiter へ渡されるマッチ確定通知。
///
/// drive は自分の `completion_rx`（game 終了通知）と、waiter の transport を受け取るための
/// `transport_responder` を両方含めて送る。waiter はこれを受け取ったら自分の transport を
/// `transport_responder` で返送し、`completion_rx` を await して終局まで待機する。
struct MatchRequest {
    /// waiter が自分の `TcpTransport` をここで返送する。
    transport_responder: oneshot::Sender<TcpTransport>,
    /// drive 側が終局後に `send(())` する。waiter はこれを受けてタスクを終える。
    completion_rx: oneshot::Receiver<()>,
}

/// 待機プール内の 1 スロット。
///
/// transport は waiter のタスクが保持し続ける（切断を検知できるようにするため）。
/// drive 側はここに入っている [`oneshot::Sender<MatchRequest>`] を通して待機側へ
/// マッチ確定を通知する。`take_complement` でプールから取り出された slot は、
/// `match_request_tx.send(...)` の成否で waiter が健在かどうか判定できる。
struct WaitingSlot {
    /// 認証後に確定した handle 単独部分（League へ登録した名前）。
    handle: String,
    /// 希望手番。
    color: Color,
    /// drive 側 → waiter への確定通知。
    match_request_tx: oneshot::Sender<MatchRequest>,
}

/// 待機プール。
///
/// `game_name` 別にキューを持ち、各キュー内で先着順に保持する。
/// drive 側は相補手番のスロットを先頭から順に探す。
#[derive(Default)]
struct WaitingPool {
    queues: HashMap<GameName, VecDeque<WaitingSlot>>,
}

impl WaitingPool {
    fn push(&mut self, game_name: GameName, slot: WaitingSlot) {
        self.queues.entry(game_name).or_default().push_back(slot);
    }

    /// 相補手番のスロットを 1 件取り出す。見つからなければ `None`。
    fn take_complement(&mut self, game_name: &GameName, want: Color) -> Option<WaitingSlot> {
        let queue = self.queues.get_mut(game_name)?;
        let idx = queue.iter().position(|s| s.color == want.opposite())?;
        queue.remove(idx)
    }

    /// 指定 handle のスロットをプールから除去する（待機中の切断検知時の掃除用）。
    fn remove_by_handle(&mut self, game_name: &GameName, handle: &str) -> bool {
        let Some(queue) = self.queues.get_mut(game_name) else {
            return false;
        };
        let Some(idx) = queue.iter().position(|s| s.handle == handle) else {
            return false;
        };
        queue.remove(idx);
        true
    }
}

/// サーバー全体で共有する状態。
pub struct SharedState<R, K, P>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    config: ServerConfig,
    league: Mutex<League>,
    waiting: Mutex<WaitingPool>,
    rate_limiter: IpLoginRateLimiter,
    broadcaster: InMemoryBroadcaster,
    rate_storage: R,
    kifu_storage: K,
    password_store: P,
    hasher: Box<dyn PasswordHasher>,
    /// 全接続タスクの終了を待つためのカウンタ通知。graceful shutdown 用 (Phase 5 で本格化)。
    active_games: Notify,
    /// 連番カウンタ（game_id 生成）。起動時刻 + 連番で衝突を避ける。
    game_counter: Mutex<u64>,
    /// サーバー起動時刻（game_id プリフィックス用）。
    started_at: chrono::DateTime<chrono::Utc>,
}

/// パスワードストアの抽象。`handle` に対応する保存ハッシュ（Phase 1 は平文）を返す。
pub trait PasswordStore {
    /// `handle` に対応する保存済みパスワードを返す。未登録なら `None`。
    fn lookup(&self, handle: &str) -> Option<String>;
}

/// メモリ常駐のテスト・開発用 PasswordStore。起動時に `HashMap` を渡す。
pub struct InMemoryPasswordStore {
    /// handle → plain password。Phase 1 は平文（shogi-server 互換）。
    pub map: HashMap<String, String>,
}

impl PasswordStore for InMemoryPasswordStore {
    fn lookup(&self, handle: &str) -> Option<String> {
        self.map.get(handle).cloned()
    }
}

/// サーバーを起動する。`bind_addr` で待ち受け、各接続を独立タスクで処理する。
///
/// 呼び出し側は [`tokio::task::LocalSet`] 内で本関数を呼ぶ必要がある。
/// port トレイトの `async fn in trait` は `Send` 境界を持たず（Cloudflare Workers の
/// シングルスレッド wasm ランタイムと互換性を取るため）、`tokio::spawn`（Send 必須）
/// では扱えないため、TCP バイナリは `current_thread` ランタイム + [`LocalSet`] 経路で
/// 配線する設計を取る。
///
/// 戻り値は accept ループのタスクハンドル。テストでは `abort()` でシャットダウンする。
pub async fn run_server<R, K, P>(
    state: Rc<SharedState<R, K, P>>,
) -> Result<JoinHandle<()>, std::io::Error>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let listener = TcpListener::bind(state.config.bind_addr).await?;
    log::info!(
        "rshogi-csa-server-tcp listening on {} ({})",
        state.config.bind_addr,
        crate::phase_gate::PhaseGate::label()
    );
    let handle = tokio::task::spawn_local(accept_loop(listener, state));
    Ok(handle)
}

/// 受理ループ。各接続を `spawn_local` で同スレッド内の独立タスクにする。
async fn accept_loop<R, K, P>(listener: TcpListener, state: Rc<SharedState<R, K, P>>)
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                log::debug!("accepted {addr}");
                let st = state.clone();
                tokio::task::spawn_local(async move {
                    if let Err(e) = handle_connection(stream, st).await {
                        log::info!("connection {addr} ended: {e:?}");
                    }
                });
            }
            Err(e) => {
                log::warn!("accept error: {e}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

/// 1 接続分の処理。LOGIN → 待機プール or drive → 終局まで。
async fn handle_connection<R, K, P>(
    stream: TcpStream,
    state: Rc<SharedState<R, K, P>>,
) -> Result<(), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let peer = TcpTransport::peer_key(&stream)?;
    let mut transport = TcpTransport::new(stream, peer.clone());

    // 1. 同一 IP からの LOGIN 試行レート制限。
    match state.rate_limiter.record(&peer).await {
        RateDecision::Allow => {}
        RateDecision::Deny { retry_after_sec } => {
            let _ = transport
                .send_line(&CsaLine::new(format!(
                    "LOGIN:incorrect rate_limited retry_after={retry_after_sec}"
                )))
                .await;
            return Ok(());
        }
    }

    // 2. LOGIN 行を受信。
    let login_line = transport.recv_line(state.config.login_timeout).await?;
    let cmd = parse_command(&login_line)?;
    let (full_name, password, _x1) = match cmd {
        ClientCommand::Login { name, password, x1 } => (name, password, x1),
        _ => {
            let _ = transport.send_line(&CsaLine::new("LOGIN:incorrect")).await;
            return Err(ServerError::Protocol(ProtocolError::Malformed(
                "first command must be LOGIN".into(),
            )));
        }
    };

    // 3. handle / game_name / color を抽出。
    let Some((handle, game_name, color)) = parse_handle(full_name.as_str()) else {
        let _ = transport.send_line(&CsaLine::new("LOGIN:incorrect")).await;
        return Err(ServerError::Protocol(ProtocolError::Malformed(format!(
            "login handle must be handle+game_name+color: `{}`",
            full_name
        ))));
    };

    // 4. パスワード照合。PasswordStore は handle 単位、RateStorage も handle で登録。
    let handle_player = PlayerName::new(&handle);
    let Some(stored_hash) = state.password_store.lookup(&handle) else {
        let _ = transport.send_line(&CsaLine::new("LOGIN:incorrect")).await;
        return Ok(());
    };
    match authenticate(
        &state.rate_storage,
        state.hasher.as_ref(),
        &handle_player,
        &password,
        &stored_hash,
    )
    .await?
    {
        AuthOutcome::Ok { .. } => {}
        AuthOutcome::Incorrect => {
            let _ = transport.send_line(&CsaLine::new("LOGIN:incorrect")).await;
            return Ok(());
        }
    }
    // LOGIN 成功応答: shogi-server 互換の `LOGIN:<handle> OK`。
    transport.send_line(&CsaLine::new(format!("LOGIN:{handle} OK"))).await?;

    // 5. League に登録し GameWaiting に遷移。
    {
        let mut league = state.league.lock().await;
        match league.login(&handle_player, false) {
            LoginResult::Ok { .. } => {}
            LoginResult::AlreadyLoggedIn => {
                let _ =
                    transport.send_line(&CsaLine::new("LOGIN:incorrect already_logged_in")).await;
                return Ok(());
            }
            LoginResult::Incorrect => {
                let _ = transport.send_line(&CsaLine::new("LOGIN:incorrect")).await;
                return Ok(());
            }
        }
        league
            .transition(
                &handle_player,
                PlayerStatus::GameWaiting {
                    game_name: game_name.clone(),
                    preferred_color: Some(color),
                },
            )
            .map_err(ServerError::State)?;
    }

    // 6. 待機プールで相補手番の相手を探す。
    //    - 相手が居れば drive 側として handoff を要求し、opp の transport を受け取って対局を駆動する。
    //      handoff に失敗（waiter が切断済み等）したら fall through して自分が waiter になる。
    //    - 相手が居なければ自分を WaitingSlot として登録し、同時に transport を持ち続けたまま
    //      マッチ確定 or 切断 を `tokio::select!` で監視する。
    if let Some(slot) = {
        let mut pool = state.waiting.lock().await;
        pool.take_complement(&game_name, color)
    } {
        // drive 側パス。
        let (resp_tx, resp_rx) = oneshot::channel::<TcpTransport>();
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let req = MatchRequest {
            transport_responder: resp_tx,
            completion_rx: done_rx,
        };
        let opp_handle = slot.handle.clone();
        let opp_color = slot.color;
        if slot.match_request_tx.send(req).is_ok()
            && let Ok(opp_transport) = resp_rx.await
        {
            return drive_game(
                state.clone(),
                opp_transport,
                opp_handle,
                opp_color,
                transport,
                handle,
                color,
                done_tx,
            )
            .await;
        }
        // waiter が直前に切断などで離脱していた場合、handoff は失敗する。
        // その場合は自分が waiter 役として待機し直す（League は GameWaiting のまま）。
        log::info!("matchmaking handoff failed for {opp_handle}; falling back to waiter");
    }

    // waiter 側パス: transport を保持したまま、マッチ確定 or 切断 を監視する。
    run_waiter(state.clone(), transport, handle, color, game_name, handle_player).await
}

/// waiter として待機プールに入り、マッチ確定 or 切断を待つ。
#[allow(clippy::too_many_arguments)]
async fn run_waiter<R, K, P>(
    state: Rc<SharedState<R, K, P>>,
    mut transport: TcpTransport,
    handle: String,
    color: Color,
    game_name: GameName,
    handle_player: PlayerName,
) -> Result<(), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let (match_req_tx, match_req_rx) = oneshot::channel::<MatchRequest>();
    {
        let mut pool = state.waiting.lock().await;
        pool.push(
            game_name.clone(),
            WaitingSlot {
                handle: handle.clone(),
                color,
                match_request_tx: match_req_tx,
            },
        );
    }

    // 切断監視: recv_line は cancel-safe（`TcpTransport::recv_line`）なので、
    // `match_req_rx` が先に発火すればバッファは保存されたまま drive 側へ transport を渡せる。
    let waiter_outcome = tokio::select! {
        req_res = match_req_rx => {
            match req_res {
                Ok(req) => {
                    // transport を drive 側へ渡し、終局通知を待つ。
                    let _ = req.transport_responder.send(transport);
                    let _ = req.completion_rx.await;
                    WaiterOutcome::Completed
                }
                Err(_) => {
                    // pool 側が破棄された。league だけクリーンアップ。
                    WaiterOutcome::Aborted
                }
            }
        }
        _res = transport.recv_line(NEAR_INFINITE) => {
            // 切断 or 待機中に不正行が来た → waiter を撤去して LOGIN をリセット。
            // （Phase 1 は GameWaiting 中のクライアント入力を受け付けないため、任意のデータを
            //  受信した時点で接続を閉じる運用とする。）
            let mut pool = state.waiting.lock().await;
            let _removed = pool.remove_by_handle(&game_name, &handle);
            WaiterOutcome::DisconnectedFromPool
        }
    };

    // 共通後処理: League から除去する。drive 側が端末処理する経路を除く。
    match waiter_outcome {
        WaiterOutcome::Completed => {
            // drive 側で end_game + logout 済み。何もしない。
        }
        WaiterOutcome::Aborted | WaiterOutcome::DisconnectedFromPool => {
            let mut league = state.league.lock().await;
            league.logout(&handle_player);
        }
    }
    state.active_games.notify_waiters();
    Ok(())
}

/// waiter タスクの終了理由。ログとクリーンアップ方針の分岐に使う。
enum WaiterOutcome {
    /// drive 側が通常終局して completion_rx が発火した（drive 側が片付け済）。
    Completed,
    /// pool から slot が落ちていた等のマッチ中断（league からだけ除去する）。
    Aborted,
    /// 対局前に切断を検知した（pool + league から除去する）。
    DisconnectedFromPool,
}

/// drive 側タスクのメインループ。両 transport を所有して 1 対局を完了まで運ぶ。
#[allow(clippy::too_many_arguments)]
async fn drive_game<R, K, P>(
    state: Rc<SharedState<R, K, P>>,
    opp_transport: TcpTransport,
    opp_handle: String,
    opp_color: Color,
    self_transport: TcpTransport,
    self_handle: String,
    self_color: Color,
    opp_completion_tx: oneshot::Sender<()>,
) -> Result<(), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    debug_assert_eq!(opp_color, self_color.opposite());

    // 役割割り当て: Black / White transport を色で確定。
    let (mut black_transport, mut white_transport, black_handle, white_handle) =
        if self_color == Color::Black {
            (self_transport, opp_transport, self_handle, opp_handle)
        } else {
            (opp_transport, self_transport, opp_handle, self_handle)
        };

    // 対局 ID を発行。
    let game_id = {
        let mut counter = state.game_counter.lock().await;
        *counter += 1;
        GameId::new(format!("{}{:04}", state.started_at.format("%Y%m%d%H%M%S"), *counter))
    };

    // League 側でペア確定 (confirm_match) → AgreeWaiting へ。
    let matched = MatchedPair {
        black: PlayerName::new(&black_handle),
        white: PlayerName::new(&white_handle),
    };
    {
        let mut league = state.league.lock().await;
        league.confirm_match(&matched, game_id.clone()).map_err(ServerError::State)?;
    }

    // confirm_match 済みの時点で League には両者が AgreeWaiting として残っている。
    // 以降のどの経路（送信失敗・切断・内部エラー）でも必ず end_game + logout を実行する
    // ため、内部処理を `drive_game_inner` に切り出し、結果を問わず epilogue で後始末する
    // （`?` の早期 return で League が解放されず再 LOGIN が詰まる経路を防ぐ）。
    let inner = drive_game_inner(
        state.as_ref(),
        &game_id,
        matched.clone(),
        &mut black_transport,
        &mut white_transport,
    )
    .await;

    // 後始末は inner の結果に関係なく必ず走る。
    {
        let mut league = state.league.lock().await;
        let _ = league.end_game(&matched);
        league.logout(&matched.black);
        league.logout(&matched.white);
    }
    state.broadcaster.clear_room(&RoomId::new(game_id.as_str())).await;
    // 待機タスクに完了通知（これで先着側のタスクが抜ける）。
    let _ = opp_completion_tx.send(());
    state.active_games.notify_waiters();
    inner
}

/// `confirm_match` 後の主処理。Game_Summary → AGREE → 対局 → 棋譜永続化までを行う。
/// 本関数は League/Pool の後始末を行わない（呼び出し側 `drive_game` が必ず実行する）。
async fn drive_game_inner<R, K, P>(
    state: &SharedState<R, K, P>,
    game_id: &GameId,
    matched: MatchedPair,
    black_transport: &mut TcpTransport,
    white_transport: &mut TcpTransport,
) -> Result<(), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    // Game_Summary を両対局者に送信。
    let clock = SecondsCountdownClock::new(state.config.total_time_sec, state.config.byoyomi_sec);
    let summary = GameSummaryBuilder {
        game_id: game_id.clone(),
        black: matched.black.clone(),
        white: matched.white.clone(),
        time_section: clock.format_summary(),
        position_section: standard_initial_position_block(),
        rematch_on_draw: false,
        to_move: Color::Black,
        declaration: "Jishogi 1.1".to_owned(),
    };
    send_multiline(black_transport, &summary.build_for(Color::Black)).await?;
    send_multiline(white_transport, &summary.build_for(Color::White)).await?;

    // 両者 AGREE を待ち合わせる。REJECT/CHUDAN/切断は対局不成立として扱う。
    let (agree_ok, _log) =
        wait_both_agree(black_transport, white_transport, game_id, state.config.agree_timeout)
            .await?;
    if !agree_ok {
        // 片方が REJECT したら両者に REJECT 行を通知する。
        let _ = black_transport.send_line(&CsaLine::new(format!("REJECT:{game_id}"))).await;
        let _ = white_transport.send_line(&CsaLine::new(format!("REJECT:{game_id}"))).await;
        return Ok(());
    }

    // 両者 InGame へ遷移させてから run_room を呼ぶ。
    {
        let mut league = state.league.lock().await;
        for n in [&matched.black, &matched.white] {
            league
                .transition(
                    n,
                    PlayerStatus::InGame {
                        game_id: game_id.clone(),
                    },
                )
                .map_err(ServerError::State)?;
        }
    }

    // 指し手と消費時間を記録するため、run_loop の代わりに handle_line を自前で回す。
    // 理由: run_room は broadcasts を transport に流し込むが、棋譜を組み立てるには
    //       手トークンと消費秒数を別途保持する必要があるため、最小の拡張として
    //       start_game_and_record を用意する。
    let start_time = chrono::Utc::now();
    let (result, moves) = start_game_and_record(
        state,
        game_id,
        matched.clone(),
        black_transport,
        white_transport,
        clock,
    )
    .await?;
    let end_time = chrono::Utc::now();

    // 棋譜 + 00LIST 永続化。
    persist_kifu(state, game_id, &matched, start_time, end_time, &moves, &result).await?;
    Ok(())
}

/// 複数行文字列（`Game_Summary` 等）を `ClientTransport::send_line` に分解して送る。
async fn send_multiline<T: ClientTransport>(
    transport: &mut T,
    blob: &str,
) -> Result<(), TransportError> {
    for line in blob.lines() {
        transport.send_line(&CsaLine::new(line)).await?;
    }
    Ok(())
}

/// 双方の AGREE 応答を待ち合わせる。REJECT/Chudan/切断時は `Ok((false, ..))` を返す。
///
/// `agree_timeout` は `Game_Summary` 送信時点から計測する **トータル** の待機窓。
/// ループ毎に `recv_line(agree_timeout)` を張り直すと片側 KEEPALIVE の連打でタイマーが
/// 際限なくリセットされ、もう一方の AGREE が無期限に待たされるため、
/// `deadline = Instant::now() + agree_timeout` を固定し、各 `recv_line` には
/// 「deadline までの残り時間」を渡す。ハードリミットに到達したら `Ok((false, ..))` で
/// 不成立として抜ける。
async fn wait_both_agree(
    black: &mut TcpTransport,
    white: &mut TcpTransport,
    game_id: &GameId,
    agree_timeout: Duration,
) -> Result<(bool, Vec<(Color, String)>), ServerError> {
    let deadline = tokio::time::Instant::now() + agree_timeout;
    let mut agreed = [false; 2]; // [Black, White]
    let mut log: Vec<(Color, String)> = Vec::new();
    while !(agreed[0] && agreed[1]) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            // トータル窓超過。select! の race や同一ソケットへの連続送信で直前に届いた
            // AGREE を取りこぼさないよう、deadline 到達時に両 transport の buffer を
            // Timeout が返るまで繰り返し非ブロッキング drain する。
            // `recv_line(Duration::ZERO)` は buffer に完全な行があれば即時返し、
            // 無ければ Timeout を返すため I/O 待ちは発生しない。
            for (c, t) in [(Color::Black, &mut *black), (Color::White, &mut *white)] {
                let idx = if matches!(c, Color::Black) { 0 } else { 1 };
                if agreed[idx] {
                    continue;
                }
                // 各 transport について、buffer が空になる (Timeout / Closed) または
                // 結果が確定するまで複数行を drain する。
                while let Ok(line) = t.recv_line(Duration::ZERO).await {
                    log.push((c, line.as_str().to_owned()));
                    match parse_command(&line)? {
                        ClientCommand::Agree { game_id: id } => {
                            if let Some(req) = id
                                && req != *game_id
                            {
                                return Ok((false, log));
                            }
                            agreed[idx] = true;
                            break; // この transport は合意取得 → 次の color へ
                        }
                        ClientCommand::Reject { .. } => return Ok((false, log)),
                        ClientCommand::KeepAlive => continue, // 同 transport でさらに続きを drain
                        _ => return Ok((false, log)),
                    }
                }
            }
            if agreed[0] && agreed[1] {
                return Ok((true, log));
            }
            return Ok((false, log));
        }
        let remaining = deadline - now;
        let evt = tokio::select! {
            r = black.recv_line(remaining) => (Color::Black, r),
            r = white.recv_line(remaining) => (Color::White, r),
        };
        match evt {
            (from, Ok(line)) => {
                log.push((from, line.as_str().to_owned()));
                let cmd = parse_command(&line)?;
                match cmd {
                    ClientCommand::Agree { game_id: id } => {
                        if let Some(req) = id
                            && req != *game_id
                        {
                            return Ok((false, log));
                        }
                        let idx = if matches!(from, Color::Black) { 0 } else { 1 };
                        agreed[idx] = true;
                    }
                    ClientCommand::Reject { .. } => return Ok((false, log)),
                    ClientCommand::KeepAlive => {}
                    _ => {
                        // AGREE 待ち中に別コマンドは protocol error にして不成立。
                        return Ok((false, log));
                    }
                }
            }
            // Timeout（deadline 到達）は不成立ではなく drain 経路へ合流させる。
            // `remaining` で recv_line が先に期限切れしても、反対側 future がキャンセルされた
            // 時点で line_buf に AGREE が残っているケースを救うため、ループ先頭の
            // deadline 分岐で drain する。
            (_, Err(TransportError::Timeout)) => continue,
            // Closed / Io 系は切断として即座に不成立。
            (_, Err(_)) => return Ok((false, log)),
        }
    }
    Ok((true, log))
}

/// 対局を駆動しつつ、棋譜記録用に各指し手の `(token, elapsed_sec)` を収集する。
///
/// `run_room` を直接使うと消費秒数を取り出せないため、この関数では `GameRoom` を
/// 直接駆動して手番イベントから `,T<sec>` を解析する。
async fn start_game_and_record<R, K, P>(
    state: &SharedState<R, K, P>,
    game_id: &GameId,
    matched: MatchedPair,
    black: &mut TcpTransport,
    white: &mut TcpTransport,
    clock: SecondsCountdownClock,
) -> Result<(GameResult, Vec<KifuMove>), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let cfg = GameRoomConfig {
        game_id: game_id.clone(),
        black: matched.black.clone(),
        white: matched.white.clone(),
        max_moves: state.config.max_moves,
        time_margin_ms: state.config.time_margin_ms,
        entering_king_rule: state.config.entering_king_rule,
    };
    let mut room = GameRoom::new(cfg, Box::new(clock));

    // broadcasts を監視して `,T<sec>` を抜き取る軽量ラッパ。run_room に相当する処理を
    // ここで再実装する。時間切れアラームも同じ方式で駆動する。
    let start = tokio::time::Instant::now();
    let now_ms = || tokio::time::Instant::now().saturating_duration_since(start).as_millis() as u64;
    // 対局開始を双方に通知するため、内部的に AGREE を 2 回入れてから Playing に進める。
    // 外部クライアントからの AGREE は wait_both_agree で受信済みなので、ここでは
    // GameRoom の内部状態だけを進める。
    room.handle_line(Color::Black, &CsaLine::new("AGREE"), now_ms())?;
    let r = room.handle_line(Color::White, &CsaLine::new("AGREE"), now_ms())?;
    dispatch(&r.broadcasts, black, white, &state.broadcaster, &RoomId::new(game_id.as_str()))
        .await?;

    // 手と時間を記録するバッファ。
    let mut recorded_moves: Vec<KifuMove> = Vec::new();

    loop {
        let status = room.status().clone();
        if let rshogi_csa_server::GameStatus::Finished(result) = status {
            return Ok((result, recorded_moves));
        }
        let deadline = compute_timeup_deadline(&room);
        // 受信側は「実質無限」で貼る。持ち時間の終端は `sleep_until(deadline)` 側で駆動する。
        // 1 時間で打ち切っていると長時間持ち時間の対局が誤って切断負けになる。
        let evt = tokio::select! {
            r = black.recv_line(NEAR_INFINITE) => Evt::Recv(Color::Black, r),
            r = white.recv_line(NEAR_INFINITE) => Evt::Recv(Color::White, r),
            _ = tokio::time::sleep_until(deadline) => Evt::TimeUp,
        };
        let r = match evt {
            Evt::Recv(from, Ok(line)) => room.handle_line(from, &line, now_ms())?,
            Evt::Recv(from, Err(TransportError::Closed | TransportError::Timeout)) => {
                room.force_abnormal(from)
            }
            Evt::Recv(_, Err(e)) => return Err(ServerError::Transport(e)),
            Evt::TimeUp => {
                let loser: Color = room.position().side_to_move().into();
                room.force_time_up(loser)
            }
        };
        // 着手行 `<token>,T<sec>` を抽出（BroadcastTarget::All で配信される）。
        for entry in &r.broadcasts {
            if let Some((tok, tsec)) = parse_move_broadcast(entry.line.as_str()) {
                recorded_moves.push(KifuMove {
                    token: CsaMoveToken::new(tok),
                    elapsed_sec: tsec,
                    comment: None,
                });
            }
        }
        dispatch(&r.broadcasts, black, white, &state.broadcaster, &RoomId::new(game_id.as_str()))
            .await?;
    }
}

enum Evt {
    Recv(Color, Result<CsaLine, TransportError>),
    TimeUp,
}

/// `run_room` と同じ dispatch ロジック（コピー。run_loop 外で使うため）。
async fn dispatch(
    entries: &[rshogi_csa_server::BroadcastEntry],
    black: &mut TcpTransport,
    white: &mut TcpTransport,
    broadcaster: &InMemoryBroadcaster,
    room_id: &RoomId,
) -> Result<(), ServerError> {
    use rshogi_csa_server::BroadcastTarget;
    for entry in entries {
        match entry.target {
            BroadcastTarget::Black => black.send_line(&entry.line).await?,
            BroadcastTarget::White => white.send_line(&entry.line).await?,
            BroadcastTarget::Players => {
                black.send_line(&entry.line).await?;
                white.send_line(&entry.line).await?;
            }
            BroadcastTarget::Spectators => {
                broadcaster.broadcast_tag(room_id, BroadcastTag::Spectator, &entry.line).await?;
            }
            BroadcastTarget::All => {
                black.send_line(&entry.line).await?;
                white.send_line(&entry.line).await?;
                broadcaster.broadcast_tag(room_id, BroadcastTag::Spectator, &entry.line).await?;
            }
        }
    }
    Ok(())
}

/// 手番側残時間 + マージン + 猶予で時間切れ deadline を算出（run_loop と同等）。
fn compute_timeup_deadline(room: &GameRoom) -> tokio::time::Instant {
    // 手番側の予算（本体 + byoyomi）で deadline を計算する。本体残時間だけを使うと
    // byoyomi 区間に入らず即 time-up してしまうバグになる。
    let side: Color = room.position().side_to_move().into();
    let turn_budget = room.clock_turn_budget_ms(side).max(0) as u64;
    let margin = room.time_margin_ms();
    tokio::time::Instant::now() + Duration::from_millis(turn_budget + margin + 250)
}

/// `<token>,T<sec>` 形式の broadcast 行を `(token, elapsed_sec)` に分解する。
fn parse_move_broadcast(line: &str) -> Option<(&str, u32)> {
    let (tok, rest) = line.split_once(',')?;
    if !(tok.starts_with('+') || tok.starts_with('-')) {
        return None;
    }
    let t = rest.strip_prefix('T')?;
    let sec: u32 = t.parse().ok()?;
    Some((tok, sec))
}

/// 棋譜 + 00LIST を永続化する。
async fn persist_kifu<R, K, P>(
    state: &SharedState<R, K, P>,
    game_id: &GameId,
    matched: &MatchedPair,
    start_time: chrono::DateTime<chrono::Utc>,
    end_time: chrono::DateTime<chrono::Utc>,
    moves: &[KifuMove],
    result: &GameResult,
) -> Result<(), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let clock = SecondsCountdownClock::new(state.config.total_time_sec, state.config.byoyomi_sec);
    let record = KifuRecord {
        game_id: game_id.clone(),
        black: matched.black.clone(),
        white: matched.white.clone(),
        start_time: start_time.format("%Y/%m/%d %H:%M:%S").to_string(),
        end_time: end_time.format("%Y/%m/%d %H:%M:%S").to_string(),
        event: "rshogi-csa-server-tcp".to_owned(),
        time_section: clock.format_summary(),
        initial_position: "PI\n+\n".to_owned(),
        moves: moves.to_vec(),
        result: result.clone(),
    };
    let csa = record.build_v2();
    state.kifu_storage.save(game_id, &csa).await.map_err(ServerError::Storage)?;
    let entry = GameSummaryEntry {
        game_id: game_id.clone(),
        sente: matched.black.clone(),
        gote: matched.white.clone(),
        start_time: start_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        end_time: end_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        // 00LIST の結果コードは core crate の `primary_result_code` を唯一の情報源として使う
        // （TCP 側との二重定義を避けて #OUTE_SENNICHITE 等の語彙方針が片側だけズレない
        // ようにする）。
        result_code: primary_result_code(result).to_owned(),
    };
    state.kifu_storage.append_summary(&entry).await.map_err(ServerError::Storage)?;
    Ok(())
}

/// `SharedState` を組み立てるヘルパ（運用コードとテストで再利用）。
#[allow(clippy::too_many_arguments)]
pub fn build_state<R, K, P>(
    config: ServerConfig,
    rate_storage: R,
    kifu_storage: K,
    password_store: P,
    hasher: Box<dyn PasswordHasher>,
    rate_limiter: IpLoginRateLimiter,
    broadcaster: InMemoryBroadcaster,
) -> SharedState<R, K, P>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    SharedState {
        config,
        league: Mutex::new(League::new()),
        waiting: Mutex::new(WaitingPool::default()),
        rate_limiter,
        broadcaster,
        rate_storage,
        kifu_storage,
        password_store,
        hasher,
        active_games: Notify::new(),
        game_counter: Mutex::new(0),
        started_at: chrono::Utc::now(),
    }
}

/// 既定の TCP サーバー構築ヘルパ。`bind_addr` と `kifu_topdir` を上書きする用途。
pub fn default_tcp_shared_state<R, P>(
    config: ServerConfig,
    rate_storage: R,
    password_store: P,
) -> SharedState<R, FileKifuStorage, P>
where
    R: RateStorage + 'static,
    P: PasswordStore + 'static,
{
    let kifu_storage = FileKifuStorage::new(config.kifu_topdir.clone());
    build_state(
        config,
        rate_storage,
        kifu_storage,
        password_store,
        Box::new(crate::auth::PlainPasswordHasher::new()),
        IpLoginRateLimiter::default_limits(),
        InMemoryBroadcaster::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_handle_accepts_black_and_white_aliases() {
        let (h, g, c) = parse_handle("alice+g1+black").unwrap();
        assert_eq!(h, "alice");
        assert_eq!(g.as_str(), "g1");
        assert_eq!(c, Color::Black);
        assert_eq!(parse_handle("bob+g1+W").unwrap().2, Color::White);
        assert_eq!(parse_handle("bob+g1+sente").unwrap().2, Color::Black);
        assert_eq!(parse_handle("bob+g1+gote").unwrap().2, Color::White);
    }

    #[test]
    fn parse_handle_rejects_malformed() {
        assert!(parse_handle("alice").is_none());
        assert!(parse_handle("alice+g1").is_none());
        assert!(parse_handle("alice+g1+black+extra").is_none());
        assert!(parse_handle("+g1+black").is_none());
        assert!(parse_handle("alice++black").is_none());
        assert!(parse_handle("alice+g1+purple").is_none());
    }

    #[test]
    fn parse_move_broadcast_extracts_sec() {
        assert_eq!(parse_move_broadcast("+7776FU,T3"), Some(("+7776FU", 3)));
        assert_eq!(parse_move_broadcast("-3334FU,T10"), Some(("-3334FU", 10)));
        assert_eq!(parse_move_broadcast("#RESIGN"), None);
        assert_eq!(parse_move_broadcast("+7776FU,Tx"), None);
    }
}
