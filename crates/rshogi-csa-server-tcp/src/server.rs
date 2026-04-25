//! TCP 受付ループと 1 接続分のセッションドライバ。
//!
//! 以下の流れを 1 タスクで駆動する:
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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use rshogi_core::types::EnteringKingRule;
use rshogi_csa_server::ClockSpec;
use rshogi_csa_server::config::{FloodgateFeatureIntent, validate_floodgate_feature_gate};
use rshogi_csa_server::error::{ProtocolError, ServerError};
use rshogi_csa_server::game::result::GameResult;
use rshogi_csa_server::game::room::{GameRoom, GameRoomConfig};
use rshogi_csa_server::matching::league::{League, LoginResult, MatchedPair, PlayerStatus};
use rshogi_csa_server::matching::registry::{GameListing, GameRegistry};
use rshogi_csa_server::port::{
    BroadcastTag, Broadcaster, BuoyStorage, ClientTransport, GameSummaryEntry, KifuStorage,
    RateDecision, RateStorage,
};
use rshogi_csa_server::protocol::command::{ClientCommand, parse_command};
use rshogi_csa_server::protocol::summary::{
    GameSummaryBuilder, position_section_from_sfen, side_to_move_from_sfen,
    standard_initial_position_block,
};
use rshogi_csa_server::record::kifu::{
    KifuMove, KifuRecord, fork_initial_sfen_from_kifu, initial_sfen_from_csa_moves,
    primary_result_code,
};
use rshogi_csa_server::types::{
    Color, CsaLine, CsaMoveToken, GameId, GameName, PlayerName, RoomId,
};
use rshogi_csa_server::{FileKifuStorage, TransportError};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify, oneshot};
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::auth::{AuthOutcome, PasswordHasher, authenticate};
use crate::broadcaster::{InMemoryBroadcaster, Subscriber};
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
    /// 対局で使う時計方式とパラメータ。
    pub clock: ClockSpec,
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
    /// x1 waiter が `%%` 系応答を 1 行送出するときの書き込みタイムアウト。
    ///
    /// x1 client が応答を読まずに詰まると、`run_waiter` の `send_line` が
    /// 無期限にブロックし、同時刻に到着した対局相手（drive 側）への transport
    /// handoff も止まる（`resp_rx.await` が永久に保留になる）。これは slow
    /// response ではなくマッチメイキング停止なので、1 行あたり上限を設けて
    /// 超過時は切断扱いにする。5 秒は「localhost / LAN の健常クライアント
    /// では十分、stall した client を抱え込み続けるには長すぎる」レンジ。
    pub x1_reply_write_timeout: Duration,
    /// 入玉ルール。既定は 24 点法。
    pub entering_king_rule: EnteringKingRule,
    /// 既定の対局開始局面 SFEN。`None` なら平手。
    ///
    /// 運用では通常 `None` (= 平手) のまま起動し、`%%FORK` / buoy 経由の対局
    /// のみ `GameRoomConfig::initial_sfen` を per-game で上書きする。本 field
    /// は `sensible_defaults` が全対局で使う既定値を設定するためにあり、テスト
    /// や特殊環境 (駒落ちサーバー等) で全対局を非平手で起動する経路で使う。
    pub initial_sfen: Option<String>,
    /// 管理者ハンドル (`%%SETBUOY` / `%%DELETEBUOY` の実行を許可する LOGIN 名)。
    ///
    /// 空の場合は誰も管理者ではなく、`%%SETBUOY` / `%%DELETEBUOY` は全て
    /// `PERMISSION_DENIED` で拒否される。`%%GETBUOYCOUNT` は参照系なので
    /// 管理者権限を要求しない。
    pub admin_handles: Vec<String>,
    /// Floodgate 運用機能の opt-in フラグ。`floodgate_intent_from_config` が
    /// 返す要求集合に何か含まれていて本フラグが `false` の場合、
    /// [`validate_floodgate_feature_gate`](rshogi_csa_server::config::validate_floodgate_feature_gate)
    /// が起動時に Err を返す。Floodgate 系機能を追加する PR は、対応する
    /// `ServerConfig` フィールドを増やしたうえで `floodgate_intent_from_config`
    /// が `true` を返すよう更新し、運用側に明示の opt-in を強制する。
    pub allow_floodgate_features: bool,
    /// SIGINT / SIGTERM 受信後に進行中対局の終了を待つ上限。超過分は未完了の
    /// まま log warning を出して切り捨てる。運用で「ローリング再起動時に対局
    /// を落とさない」ためのバッファで、既定 60 秒。
    pub shutdown_grace: Duration,
}

impl ServerConfig {
    /// 動作確認用の控えめな既定値。運用では `bind_addr` と `kifu_topdir` を書き換える。
    pub fn sensible_defaults() -> Self {
        Self {
            bind_addr: "127.0.0.1:4081".parse().unwrap(),
            kifu_topdir: std::path::PathBuf::from("./kifu"),
            clock: ClockSpec::default(),
            time_margin_ms: 1_500,
            max_moves: 256,
            login_timeout: Duration::from_secs(30),
            agree_timeout: Duration::from_secs(5 * 60),
            x1_reply_write_timeout: Duration::from_secs(5),
            entering_king_rule: EnteringKingRule::Point24,
            initial_sfen: None,
            admin_handles: Vec::new(),
            allow_floodgate_features: false,
            shutdown_grace: Duration::from_secs(60),
        }
    }
}

/// `ServerConfig` から「この起動構成が要求している Floodgate 系機能集合」を
/// 導出する単一窓口。
///
/// 現状は Floodgate 系設定フィールドが `ServerConfig` に存在しないため常に
/// 既定（空集合）を返し、`allow_floodgate_features` が `false` でも起動が
/// 通る。Floodgate 機能を導入する PR は次の手順で配線する:
///
/// 1. 新フィールド（例: スケジュール宣言・非 direct ペアリング戦略・重複ログイン
///    方針など）を `ServerConfig` に追加する。
/// 2. 当ヘルパで該当フィールドが「機能を要求している」状態を検出し、対応する
///    [`FloodgateFeatureIntent`] フラグを `true` にして返す。
/// 3. CLI / config 経由の入力で該当フィールドが埋まり、かつ
///    `allow_floodgate_features = false` の場合は `prepare_runtime` が
///    `validate_floodgate_feature_gate` 経由で起動失敗させる。
///
/// この単一窓口を経由することで、Floodgate 機能の追加 PR がゲート呼び出しを
/// 忘れる事故を構造的に防ぐ。
///
/// `pub(crate)` に閉じているのは「単一窓口を迂回した直接呼び出し」を型システムで
/// 防ぐため。クレート外（`bin/main.rs` 含む）からは [`prepare_runtime`] のみが
/// 入口になる。
pub(crate) fn floodgate_intent_from_config(config: &ServerConfig) -> FloodgateFeatureIntent {
    // スタブ: 現状 `ServerConfig` に Floodgate 系フィールドが存在しないため、
    // `config` を観測する必要がない。Floodgate 機能を実装する PR がフィールドを
    // 追加した時点で本関数を更新するので、その更新漏れを `let _` の存在が
    // リマインダになる（フィールド参照を追記する際に削除する）。
    let _ = config;
    FloodgateFeatureIntent::default()
}

/// 起動前に opt-in ゲートを評価する。
///
/// `floodgate_intent_from_config` が返す要求集合と `config.allow_floodgate_features`
/// を [`validate_floodgate_feature_gate`] に通し、要求があるのにフラグが立って
/// いない場合は `Err` を返して fail-fast する。CLI / バイナリは `build_state`
/// より前に本関数を呼ぶこと。
pub fn prepare_runtime(config: &ServerConfig) -> Result<(), String> {
    let intent = floodgate_intent_from_config(config);
    validate_floodgate_feature_gate(config.allow_floodgate_features, intent)
}

/// graceful shutdown 用トリガ。SIGINT / SIGTERM 受信で `trigger` され、
/// accept ループや待機 waiter が `wait()` を `tokio::select!` に組み込んで
/// cancellation を検知する。
///
/// 現在は `current_thread` ランタイム + `LocalSet` 前提で `Rc` 共有するが、
/// 同期プリミティブは `AtomicBool` + `Notify` で組んであるので、他ランタイム
/// へ移す場合も追加改修なしで使える。メモリオーダリングは `Release` (swap) /
/// `Acquire` (load) で十分で、`Notify` 側のバリアと合わせて
/// trigger → wait の happens-before 関係を維持する。
pub struct GracefulShutdown {
    triggered: AtomicBool,
    notify: Notify,
}

impl GracefulShutdown {
    /// 未トリガ状態のインスタンスを返す。
    pub(crate) fn new() -> Self {
        Self {
            triggered: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    /// シャットダウンを開始する。複数回呼ばれても冪等。main の signal ハンドラ
    /// とテストから呼ばれる。
    pub fn trigger(&self) {
        if !self.triggered.swap(true, Ordering::Release) {
            // 待機中の全 waiter に通知。新しく `wait()` してくる経路は
            // 下の `is_triggered` チェックで即座に抜ける。
            self.notify.notify_waiters();
        }
    }

    /// 既にトリガ済みか。クレート内で `wait()` の lost-wakeup ガードに使う。
    pub(crate) fn is_triggered(&self) -> bool {
        self.triggered.load(Ordering::Acquire)
    }

    /// トリガされるまで待機する。トリガ済みなら即座に返る。accept ループと
    /// waiter タスクが `tokio::select!` ブランチで使う内部 API。
    pub(crate) async fn wait(&self) {
        if self.is_triggered() {
            return;
        }
        // notify_waiters は現在待機中の全 waiter にのみ通知するため、
        // notified 登録 → atomic 再確認で lost-wakeup を避ける。
        let notified = self.notify.notified();
        if self.is_triggered() {
            return;
        }
        notified.await;
    }
}

impl Default for GracefulShutdown {
    fn default() -> Self {
        Self::new()
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
    /// 進行中対局のメモリ内レジストリ。`%%LIST` / `%%SHOW` 応答で参照する。
    ///
    /// **注意**: このカウントは graceful shutdown の完了判定に使ってはならない。
    /// `drive_game_inner` が `persist_kifu` より先に `unregister` を呼ぶため、
    /// 棋譜 flush 中に件数 0 と誤判定され得る。shutdown 判定には
    /// [`Self::active_drive_tasks`] を使う（`drive_game` epilogue の最後で
    /// デクリメントされる）。
    games: Mutex<GameRegistry>,
    /// `drive_game` タスクの在籍カウンタ。`drive_game` の entry で +1、epilogue
    /// の最後（`persist_kifu` 完了を含む全後始末の後）に -1 される。graceful
    /// shutdown の「対局完了待ち」はこのカウンタを 0 まで落とすのを待つ。
    /// `GameRegistry` の件数を使うと `persist_kifu` 中に 0 と判定される race
    /// があるため、こちらを唯一の真実とする。
    active_drive_tasks: AtomicUsize,
    /// 対局 1 件が終了（`drive_game` の epilogue 完了）したことを通知する。
    /// graceful shutdown ループがこれで起床して `active_drive_tasks` を再確認
    /// する。`run_waiter` からも呼ばれるので spurious wake が起き得るが、
    /// 起床後に counter を再確認するので正しく判定できる。
    active_games: Notify,
    /// 連番カウンタ（game_id 生成）。起動時刻 + 連番で衝突を避ける。
    game_counter: Mutex<u64>,
    /// サーバー起動時刻（game_id プリフィックス用）。
    started_at: chrono::DateTime<chrono::Utc>,
    /// ブイ (途中局面テンプレート) の永続化先。
    ///
    /// `config.kifu_topdir` 配下の `buoys/` ディレクトリを使う。TCP サーバー
    /// は常に同一プロセス・同一プロセス内で単一インスタンスを保持する前提
    /// (複数プロセス並行書き込みは非対応)。
    buoy_storage: rshogi_csa_server::FileBuoyStorage,
    /// SIGINT / SIGTERM 由来の graceful shutdown トリガ。accept ループと
    /// 待機 waiter が監視して、新規受付停止と待機セッション切断を行う。
    pub shutdown: GracefulShutdown,
}

impl<R, K, P> SharedState<R, K, P>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    /// 起動時に渡した [`ServerConfig`] を参照する。graceful shutdown などで
    /// `shutdown_grace` のような設定値を読むために使う。
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// 進行中の `drive_game` タスク数。`persist_kifu` を含む epilogue が完了
    /// して初めて 0 になる。graceful shutdown 完了判定はこのカウンタを使う。
    pub fn active_game_count(&self) -> usize {
        self.active_drive_tasks.load(Ordering::Acquire)
    }

    /// `drive_game` epilogue 完了と `run_waiter` 終了のどちらでも起床する通知。
    /// 呼び出し側は起床後に [`Self::active_game_count`] を再確認してから
    /// `break` すること（run_waiter 終了時の wake は counter を減らさないため
    /// spurious に見える）。
    ///
    /// 戻り型は `impl Future` でラップして内部で使う `Notify` の詳細を漏らさない。
    /// 将来 notify 実装を差し替える際の破壊的変更を避ける。
    pub fn wait_active_games_notify(&self) -> impl std::future::Future<Output = ()> + '_ {
        self.active_games.notified()
    }
}

/// パスワードストアの抽象。`handle` に対応する保存ハッシュ（現状は平文）を返す。
pub trait PasswordStore {
    /// `handle` に対応する保存済みパスワードを返す。未登録なら `None`。
    fn lookup(&self, handle: &str) -> Option<String>;
}

/// メモリ常駐のテスト・開発用 PasswordStore。起動時に `HashMap` を渡す。
pub struct InMemoryPasswordStore {
    /// handle → plain password。shogi-server 互換の平文保存。
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
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        bind = %state.config.bind_addr,
        "rshogi-csa-server-tcp listening"
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
    // 接続ごとに `conn_id` を採番し、tracing span のフィールドとして全ログイベント
    // に伝播する。プロセス再起動でリセットされる単純な `AtomicU64` で十分（同一
    // run 内で uniq・stable・短い表現の 3 条件を満たす）。
    let connection_seq = Rc::new(AtomicU64::new(1));
    loop {
        tokio::select! {
            // graceful shutdown 中は新規受付を止める。listener は drop されて
            // port が解放されるまでの short window では SYN が失敗する可能性が
            // あるが、既存接続と進行中対局には影響しない。
            _ = state.shutdown.wait() => {
                tracing::info!("accept loop received shutdown signal; stopping new connections");
                break;
            }
            res = listener.accept() => {
                match res {
                    Ok((stream, addr)) => {
                        let conn_id = connection_seq.fetch_add(1, Ordering::Relaxed);
                        // `game_id` は対局確定時 (`drive_game` 内) に
                        // `Span::current().record("game_id", ...)` で後から埋める
                        // 想定で、conn span 上に Empty で予約しておく。span の
                        // フィールド名は `id` ではなく `conn_id` にして、ログ
                        // shipper クエリで対局 id 等の他キーと衝突しない名前を
                        // 採用する。
                        let span = tracing::info_span!(
                            "conn",
                            conn_id = conn_id,
                            remote = %addr,
                            game_id = tracing::field::Empty,
                        );
                        span.in_scope(|| tracing::debug!("accepted"));
                        let st = state.clone();
                        tokio::task::spawn_local(
                            async move {
                                if let Err(e) = handle_connection(stream, st).await {
                                    tracing::info!(error = ?e, "connection ended");
                                }
                            }
                            .instrument(span),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept error");
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
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
    let (full_name, password, x1) = match cmd {
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

    // 5. League に登録して GameWaiting に遷移する。x1 フラグはプロトコル拡張
    //    「このクライアントは `%%` 系コマンドも解釈できる」ことを示す属性で、
    //    matchmaking への参加可否とは独立。x1 付きクライアントは通常どおり
    //    matchmaking に参加しつつ、待機中は `%%` 系コマンドを発行できる
    //    （shogi-server 互換の運用）。観戦専用で接続したいクライアントは
    //    `%%MONITOR2ON` 経路（後続のコミットで追加）を使う。
    {
        let mut league = state.league.lock().await;
        match league.login(&handle_player, x1) {
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
        // buoy を予約する前に相手 waiter の健在と transport handoff を確定する。
        // 先に予約してしまうと、相手が直前に切断していた場合に buoy 残数が
        // 消費されたまま復元されない問題があった (codex cloud P1 /
        // codex CLI round 3 P2)。
        let (resp_tx, resp_rx) = oneshot::channel::<TcpTransport>();
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let req = MatchRequest {
            transport_responder: resp_tx,
            completion_rx: done_rx,
        };
        let opp_handle = slot.handle.clone();
        let opp_color = slot.color;
        let handoff_ok = slot.match_request_tx.send(req).is_ok();
        let opp_transport = if handoff_ok { resp_rx.await.ok() } else { None };
        if let Some(opp_transport) = opp_transport {
            // handoff が確定した後で buoy を予約する。buoy が存在しない場合は
            // 通常対局、存在して残数があれば予約、残数 0 なら両者に通知して
            // 対局を取り消す。
            let match_initial_sfen =
                match reserve_match_initial_position(state.as_ref(), &game_name).await? {
                    MatchInitialPosition::Default(sfen) => sfen,
                    MatchInitialPosition::Reserved(sfen) => Some(sfen),
                    MatchInitialPosition::Exhausted => {
                        // buoy 残数 0。相手の waiter に Abort を送りたいが、既に
                        // Start を送って transport まで受け取ってしまっているので
                        // 直接 transport にエラーを送って切断する。自分も同じ
                        // エラーを送って終わる。再キューしない（silently ハング
                        // するのを避ける）。
                        tracing::info!(%game_name, "buoy exhausted after handoff; aborting match");
                        let err_line =
                            CsaLine::new(format!("##[ERROR] buoy '{game_name}' exhausted"));
                        let _ = transport.send_line(&err_line).await;
                        let mut opp_transport = opp_transport;
                        let _ = opp_transport.send_line(&err_line).await;
                        let _ = done_tx.send(());
                        // 両者の League エントリを片付ける。
                        let mut league = state.league.lock().await;
                        league.logout(&handle_player);
                        league.logout(&PlayerName::new(opp_handle.as_str()));
                        return Ok(());
                    }
                };
            return drive_game(
                state.clone(),
                opp_transport,
                opp_handle,
                opp_color,
                transport,
                handle,
                color,
                game_name.clone(),
                match_initial_sfen,
                done_tx,
            )
            .await;
        }
        // waiter が直前に切断などで離脱していた場合、handoff は失敗する。
        // その場合は自分が waiter 役として待機し直す（League は GameWaiting のまま）。
        tracing::info!(opponent = %opp_handle, "matchmaking handoff failed; falling back to waiter");
    }

    // waiter 側パス: transport を保持したまま、マッチ確定 or 切断 を監視する。
    run_waiter(state.clone(), transport, handle, color, game_name, handle_player, x1).await
}

/// waiter として待機プールに入り、マッチ確定 / 切断 / `%%` 系情報コマンドを監視する。
///
/// - 非 x1 waiter は待機中のクライアント入力を受け付けず、任意のデータ到着を
///   切断として扱う（対局前の不正入力は拒否する方針）。
/// - x1 waiter は `%%VERSION` / `%%HELP` / `%%WHO` / `%%LIST` / `%%SHOW` /
///   空行 keep-alive に応答し、それ以外の入力で切断する。マッチングへの参加は
///   非 x1 と同じ経路なので、相補手番の相手が到着すれば drive 側へ handoff する。
#[allow(clippy::too_many_arguments)]
async fn run_waiter<R, K, P>(
    state: Rc<SharedState<R, K, P>>,
    mut transport: TcpTransport,
    handle: String,
    color: Color,
    game_name: GameName,
    handle_player: PlayerName,
    x1: bool,
) -> Result<(), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let (match_req_tx, mut match_req_rx) = oneshot::channel::<MatchRequest>();
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

    // `%%MONITOR2ON <game_id>` で購読中の対局があれば、その broadcast 受信口を
    // `(game_id, Receiver<CsaLine>)` (bounded) として保持する。単一購読モデル:
    // 後続の `%%MONITOR2ON` は既存 rx を drop して差し替える。CSA x1 仕様上
    // 複数同時観戦は稀なので、複雑なキュー/セット管理を避ける。
    //
    // キュー容量は `crate::broadcaster::SUBSCRIBER_CHANNEL_CAPACITY`。slow
    // consumer がキューを溜め込んだ時点で broadcaster 側が prune するため、
    // 無制限 memory 溜め込み経路 (Codex review PR #469 P2) を遮断する。
    let mut monitor_rx: Option<(GameId, tokio::sync::mpsc::Receiver<CsaLine>)> = None;

    // `%%MONITOR2ON` が成立したら観戦者扱いとなるため、waiting pool から除外する
    // 必要がある (Codex review PR #469 P1: 観戦者が同一 game_name + 相補色で
    // 後続 LOGIN とマッチさせられる経路を塞ぐ)。`pool.remove_by_handle` は
    // 冪等 (未登録なら何もしない) なので、複数回呼んでも害が無い。
    let mut observer_mode = false;

    // マッチ確定 / 受信 / 観戦 broadcast 中継 の 3 経路を監視する。x1 waiter のみ
    // 受信行を `%%` 系コマンドとして解釈し応答を返すため、recv ブランチは loop で
    // 回す。`recv_line` は cancel-safe（`TcpTransport::recv_line`）なので、マッチが
    // 先に到着した場合はバッファを保ったまま drive 側へ transport を渡せる。
    let waiter_outcome: WaiterOutcome = 'outer: loop {
        let recv = tokio::select! {
            // graceful shutdown: 待機中のセッションに `##[NOTICE] ...` を送って
            // 切断する。プレイヤー側は LOGIN 済みだが対局には入っていないので、
            // 安全に接続を閉じてプロセス終了を待てる。
            //
            // observer_mode の waiter が持っている `monitor_rx` は通常切断経路と
            // 同じく take() + prune_closed() する。こうしないと broadcaster に
            // dead sender が残って同 room の後続観戦者 / 終局 clear_room まで
            // 掃除されない。
            _ = state.shutdown.wait() => {
                let _ = transport
                    .send_line(&CsaLine::new("##[NOTICE] server shutting down"))
                    .await;
                {
                    let mut pool = state.waiting.lock().await;
                    let _ = pool.remove_by_handle(&game_name, &handle);
                }
                if let Some((room, _)) = monitor_rx.take() {
                    state.broadcaster.prune_closed(&RoomId::new(room.as_str())).await;
                }
                break 'outer WaiterOutcome::DisconnectedFromPool;
            }
            // observer_mode 時は `match_req_rx` の Err は自分が pool から自主的に
            // 外れたことが原因。`recv_line` / `forwarded` ブランチを使い続けられるよう、
            // pending() に切り替えて本ブランチを実質無効化する。`match_req_rx` を
            // `Option` 化すると `tokio::select!` 内部の pin 要件が面倒になるため、
            // ブランチ側で observer_mode 判定をする。
            req_res = async {
                if observer_mode {
                    std::future::pending::<Result<MatchRequest, oneshot::error::RecvError>>().await
                } else {
                    (&mut match_req_rx).await
                }
            } => {
                match req_res {
                    Ok(req) => {
                        // transport を drive 側へ渡し、終局通知を待つ。
                        let _ = req.transport_responder.send(transport);
                        let _ = req.completion_rx.await;
                        break 'outer WaiterOutcome::Completed;
                    }
                    Err(_) => {
                        // pool 側が破棄された。league だけクリーンアップ。
                        break 'outer WaiterOutcome::Aborted;
                    }
                }
            }
            // 観戦購読中のみ有効になる broadcast 中継経路。`monitor_rx` が `None` なら
            // `pending()` で永久に await し、実質このブランチは無効化される。
            forwarded = async {
                match &mut monitor_rx {
                    Some((_, rx)) => rx.recv().await,
                    None => std::future::pending::<Option<CsaLine>>().await,
                }
            } => {
                match forwarded {
                    Some(line) => {
                        // 観戦者向け broadcast を transport に中継。書き込み失敗・
                        // タイムアウトは切断扱い（既存の返信経路と同じ `x1_reply_write_timeout`
                        // を共用し、観戦中継がハングしてマッチメイクを止めないようにする）。
                        let write_timeout = state.config.x1_reply_write_timeout;
                        match tokio::time::timeout(write_timeout, transport.send_line(&line)).await
                        {
                            Ok(Ok(())) => continue 'outer,
                            _ => {
                                let mut pool = state.waiting.lock().await;
                                let _ = pool.remove_by_handle(&game_name, &handle);
                                break 'outer WaiterOutcome::DisconnectedFromPool;
                            }
                        }
                    }
                    None => {
                        // 送信側 (broadcaster 側の Subscriber tx) が drop された。
                        // 対局終了による `clear_room` 経由が典型。購読状態をクリアして
                        // 次の `%%MONITOR2ON` を待つ。
                        monitor_rx = None;
                        continue 'outer;
                    }
                }
            }
            recv = transport.recv_line(NEAR_INFINITE) => recv,
        };

        let line = match recv {
            Ok(l) => l,
            Err(_) => {
                // 切断 or I/O エラー → pool を抜けて終了。observer モードで
                // MONITOR2OFF を呼ばずに切断した接続は `monitor_rx` を drop する
                // ことで tx が `is_closed` になるが、`broadcaster.inner` の entry
                // は次の broadcast / subscribe / clear_room まで掃除されない。
                // broadcast が発生しない idle room で再接続を繰り返されると
                // dead sender が蓄積するため、切断時にも明示的に prune する
                // (Codex review PR #469 3rd round P2)。
                let mut pool = state.waiting.lock().await;
                let _removed = pool.remove_by_handle(&game_name, &handle);
                drop(pool);
                if let Some((room, _)) = monitor_rx.take() {
                    state.broadcaster.prune_closed(&RoomId::new(room.as_str())).await;
                }
                break 'outer WaiterOutcome::DisconnectedFromPool;
            }
        };

        if !x1 {
            // 非 x1 waiter は待機中の入力を許容しない（現行方針）。
            let mut pool = state.waiting.lock().await;
            let _removed = pool.remove_by_handle(&game_name, &handle);
            break 'outer WaiterOutcome::DisconnectedFromPool;
        }

        // x1 waiter: 情報コマンドだけ応答する。
        let cmd = match parse_command(&line) {
            Ok(c) => c,
            Err(_) => {
                // パース不能な行は切断扱い。
                let mut pool = state.waiting.lock().await;
                let _removed = pool.remove_by_handle(&game_name, &handle);
                break 'outer WaiterOutcome::DisconnectedFromPool;
            }
        };
        let replies: Option<Vec<CsaLine>> = match cmd {
            ClientCommand::KeepAlive => Some(Vec::new()),
            ClientCommand::Version => Some(rshogi_csa_server::protocol::info::version_lines()),
            ClientCommand::Help => Some(rshogi_csa_server::protocol::info::help_lines()),
            ClientCommand::Who => {
                let snapshot = {
                    let league = state.league.lock().await;
                    league.who()
                };
                Some(rshogi_csa_server::protocol::info::who_lines(&snapshot))
            }
            ClientCommand::List => {
                let snapshot = {
                    let games = state.games.lock().await;
                    games.snapshot()
                };
                Some(rshogi_csa_server::protocol::info::list_lines(&snapshot))
            }
            ClientCommand::Show { game_id } => {
                let listing = {
                    let games = state.games.lock().await;
                    games.get(&game_id).cloned()
                };
                Some(rshogi_csa_server::protocol::info::show_lines(&game_id, listing.as_ref()))
            }
            ClientCommand::Monitor2On { game_id } => {
                // 対局が GameRegistry に存在しているときのみ購読を許可する。
                let exists = {
                    let games = state.games.lock().await;
                    games.get(&game_id).is_some()
                };
                if !exists {
                    Some(vec![
                        CsaLine::new(format!("##[MONITOR2] NOT_FOUND {game_id}")),
                        CsaLine::new("##[MONITOR2] END"),
                    ])
                } else if !observer_mode {
                    // 初回の observer 転換。subscribe().await の前に waiting pool
                    // から自分を除外する必要がある。そうしないと drive 側の
                    // `take_complement` と subscribe() の await の間にレースが発生し、
                    // drive が slot を掴んだ後で我々が observer_mode に入ると
                    // match_request が監視外に流れて相手が永久 hang する
                    // (Codex review PR #469 P1)。
                    //
                    // 競合の結果は pool の Mutex で直列化されるので、`remove_by_handle`
                    // の戻り値で「先に drive が slot を掴んだか」を確実に判別できる:
                    // - true: 我々が先に取り除いた。drive は以後 slot を見つけない。
                    //         安全に observer へ遷移。
                    // - false: drive が先に slot を取っていった。match_request が
                    //         間もなく match_req_rx に届く。observer にはならず、
                    //         client に BUSY を返して通常 waiter として match_req_rx
                    //         を次のループで受けさせる。
                    let mut pool = state.waiting.lock().await;
                    let removed = pool.remove_by_handle(&game_name, &handle);
                    drop(pool);
                    if !removed {
                        Some(vec![
                            CsaLine::new(format!("##[MONITOR2] BUSY {game_id}")),
                            CsaLine::new("##[MONITOR2] END"),
                        ])
                    } else {
                        // League も `GameWaiting` → `Connected` へ戻して `%%WHO` から
                        // `waiting:<game_name>` を消す。`transition` は「未ログイン」
                        // 「Finished」でのみ Err を返すが、ここではどちらでもない。
                        let mut league = state.league.lock().await;
                        let _ = league.transition(&handle_player, PlayerStatus::Connected);
                        drop(league);
                        observer_mode = true;
                        // subscriber 登録。subscribe は内部で dead entry を prune する
                        // ため、切替や MONITOR2OFF の蓄積は O(生存購読者数) に抑えられる
                        // (Codex review PR #469 P2)。
                        let (tx, rx) = tokio::sync::mpsc::channel(
                            crate::broadcaster::SUBSCRIBER_CHANNEL_CAPACITY,
                        );
                        state
                            .broadcaster
                            .subscribe(RoomId::new(game_id.as_str()), Subscriber::new(tx))
                            .await;
                        // TOCTOU 回避: 初回 exists 確認から subscribe までの間に
                        // drive が終局して `unregister + clear_room` を完了している
                        // 可能性がある。その場合は broadcaster に stale room が残り、
                        // 観戦者は二度と broadcast を受け取れない。subscribe 後に
                        // もう一度存在確認し、消えていれば rx を drop + prune して
                        // NOT_FOUND を返す (Codex review PR #469 3rd round P2)。
                        let still_exists = subscribe_still_registered(&state, &game_id).await;
                        if !still_exists {
                            drop(rx);
                            state.broadcaster.prune_closed(&RoomId::new(game_id.as_str())).await;
                            // 状態巻き戻し: pool から抜けた + League を Connected に
                            // 遷移した + observer_mode を立てた 3 点を元に戻す。
                            // 新しい oneshot ペアを作って slot を再登録し、次の
                            // tokio::select! で match_req_rx を再び監視できる状態に
                            // 戻す (Codex review PR #469 4th round P2)。
                            let (new_tx, new_rx) = oneshot::channel::<MatchRequest>();
                            {
                                let mut pool = state.waiting.lock().await;
                                pool.push(
                                    game_name.clone(),
                                    WaitingSlot {
                                        handle: handle.clone(),
                                        color,
                                        match_request_tx: new_tx,
                                    },
                                );
                            }
                            {
                                let mut league = state.league.lock().await;
                                let _ = league.transition(
                                    &handle_player,
                                    PlayerStatus::GameWaiting {
                                        game_name: game_name.clone(),
                                        preferred_color: Some(color),
                                    },
                                );
                            }
                            match_req_rx = new_rx;
                            observer_mode = false;
                            Some(vec![
                                CsaLine::new(format!("##[MONITOR2] NOT_FOUND {game_id}")),
                                CsaLine::new("##[MONITOR2] END"),
                            ])
                        } else {
                            monitor_rx = Some((game_id.clone(), rx));
                            Some(vec![
                                CsaLine::new(format!("##[MONITOR2] BEGIN {game_id}")),
                                CsaLine::new("##[MONITOR2] END"),
                            ])
                        }
                    }
                } else {
                    // 既に observer モード。旧 rx を drop して差し替える。
                    // 差し替え前に旧 room の dead entry を明示的に prune する
                    // (subscribe 内の prune は新 room に対してのみ行われるため)。
                    if let Some((old_id, _)) = monitor_rx.take() {
                        state.broadcaster.prune_closed(&RoomId::new(old_id.as_str())).await;
                    }
                    let (tx, rx) =
                        tokio::sync::mpsc::channel(crate::broadcaster::SUBSCRIBER_CHANNEL_CAPACITY);
                    state
                        .broadcaster
                        .subscribe(RoomId::new(game_id.as_str()), Subscriber::new(tx))
                        .await;
                    // 同じく subscribe 後に TOCTOU 再確認。
                    let still_exists = subscribe_still_registered(&state, &game_id).await;
                    if !still_exists {
                        drop(rx);
                        state.broadcaster.prune_closed(&RoomId::new(game_id.as_str())).await;
                        Some(vec![
                            CsaLine::new(format!("##[MONITOR2] NOT_FOUND {game_id}")),
                            CsaLine::new("##[MONITOR2] END"),
                        ])
                    } else {
                        monitor_rx = Some((game_id.clone(), rx));
                        Some(vec![
                            CsaLine::new(format!("##[MONITOR2] BEGIN {game_id}")),
                            CsaLine::new("##[MONITOR2] END"),
                        ])
                    }
                }
            }
            ClientCommand::Monitor2Off { game_id } => {
                // 現在購読中かつ game_id が一致する場合のみ解除する。別 game_id
                // を指定された場合は no-op で OK を返す（CSA 仕様の寛容性を優先）。
                if let Some((active_id, _)) = &monitor_rx
                    && *active_id == game_id
                {
                    monitor_rx = None;
                    // 旧 rx が drop された時点で tx は is_closed になる。broadcast
                    // が起きない idle room でも tx が貯まらないよう、ここで明示的に
                    // prune する (Codex review PR #469 P2)。
                    state.broadcaster.prune_closed(&RoomId::new(game_id.as_str())).await;
                }
                Some(vec![
                    CsaLine::new(format!("##[MONITOR2OFF] {game_id}")),
                    CsaLine::new("##[MONITOR2OFF] END"),
                ])
            }
            ClientCommand::Chat { message } => {
                // 現在観戦中のルーム（`monitor_rx` が握っている game_id）に対し、
                // `##[CHAT] <handle>: <message>` 形式で同ルームの全観戦者へ broadcast
                // する。対局者 (drive 側 transport) は本 broadcaster では購読しない
                // 構成なので現時点では受信しない (制約)。対局者側への配信は後続タスク
                // で `broadcast_room` の配線を見直す際に追加する。
                //
                // 観戦中でない CHAT は NOT_MONITORING で弾く。対局参加前の x1 クライアント
                // が部屋未指定で CHAT を投げた場合のフォールバック経路。
                if let Some((active_id, _)) = &monitor_rx {
                    let line = CsaLine::new(format!("##[CHAT] {handle}: {message}"));
                    // CHAT broadcast 自体は送信側 (本クライアント) 自身にも echo
                    // として届く (broadcaster に自身が subscribe している)。これは
                    // CSA 仕様の通例で、送信確認を兼ねる。
                    let _ = state
                        .broadcaster
                        .broadcast_tag(
                            &RoomId::new(active_id.as_str()),
                            BroadcastTag::Spectator,
                            &line,
                        )
                        .await;
                    Some(vec![
                        CsaLine::new(format!("##[CHAT] OK {active_id}")),
                        CsaLine::new("##[CHAT] END"),
                    ])
                } else {
                    Some(vec![
                        CsaLine::new("##[CHAT] NOT_MONITORING"),
                        CsaLine::new("##[CHAT] END"),
                    ])
                }
            }
            ClientCommand::SetBuoy {
                game_name: buoy_name,
                moves,
                count,
            } => {
                // 管理者のみ許可。`admin_handles` リストに現ハンドルが含まれるか確認。
                // 配列 (Vec) 線形走査だが admin は通常数件なので実運用で問題にならない。
                if !state.config.admin_handles.iter().any(|h| h == &handle) {
                    Some(vec![
                        CsaLine::new(format!("##[SETBUOY] PERMISSION_DENIED {buoy_name}")),
                        CsaLine::new("##[SETBUOY] END"),
                    ])
                } else {
                    match initial_sfen_from_csa_moves(&moves) {
                        Ok(derived_initial_sfen) => match state
                            .buoy_storage
                            .store(&buoy_name, moves, count, Some(derived_initial_sfen))
                            .await
                        {
                            Ok(()) => Some(vec![
                                CsaLine::new(format!("##[SETBUOY] OK {buoy_name} {count}")),
                                CsaLine::new("##[SETBUOY] END"),
                            ]),
                            Err(e) => Some(vec![
                                CsaLine::new(format!("##[SETBUOY] ERROR {buoy_name} {e}")),
                                CsaLine::new("##[SETBUOY] END"),
                            ]),
                        },
                        Err(e) => Some(vec![
                            CsaLine::new(format!("##[SETBUOY] ERROR {buoy_name} {e}")),
                            CsaLine::new("##[SETBUOY] END"),
                        ]),
                    }
                }
            }
            ClientCommand::DeleteBuoy {
                game_name: buoy_name,
            } => {
                if !state.config.admin_handles.iter().any(|h| h == &handle) {
                    Some(vec![
                        CsaLine::new(format!("##[DELETEBUOY] PERMISSION_DENIED {buoy_name}")),
                        CsaLine::new("##[DELETEBUOY] END"),
                    ])
                } else {
                    match state.buoy_storage.delete(&buoy_name).await {
                        Ok(()) => Some(vec![
                            CsaLine::new(format!("##[DELETEBUOY] OK {buoy_name}")),
                            CsaLine::new("##[DELETEBUOY] END"),
                        ]),
                        Err(e) => Some(vec![
                            CsaLine::new(format!("##[DELETEBUOY] ERROR {buoy_name} {e}")),
                            CsaLine::new("##[DELETEBUOY] END"),
                        ]),
                    }
                }
            }
            ClientCommand::GetBuoyCount {
                game_name: buoy_name,
            } => {
                // 参照系なので権限チェックなし (全クライアントが参照可能)。
                match state.buoy_storage.count(&buoy_name).await {
                    Ok(Some(n)) => Some(vec![
                        CsaLine::new(format!("##[GETBUOYCOUNT] {buoy_name} {n}")),
                        CsaLine::new("##[GETBUOYCOUNT] END"),
                    ]),
                    Ok(None) => Some(vec![
                        CsaLine::new(format!("##[GETBUOYCOUNT] NOT_FOUND {buoy_name}")),
                        CsaLine::new("##[GETBUOYCOUNT] END"),
                    ]),
                    Err(e) => Some(vec![
                        CsaLine::new(format!("##[GETBUOYCOUNT] ERROR {buoy_name} {e}")),
                        CsaLine::new("##[GETBUOYCOUNT] END"),
                    ]),
                }
            }
            ClientCommand::Fork {
                source_game,
                new_buoy,
                nth_move,
            } => {
                let buoy_name =
                    new_buoy.unwrap_or_else(|| default_fork_buoy_name(&source_game, nth_move));
                match derive_fork_from_source_kifu(state.as_ref(), &source_game, nth_move).await? {
                    ForkOutcome::NotFound => Some(vec![
                        CsaLine::new(format!("##[FORK] NOT_FOUND {source_game}")),
                        CsaLine::new("##[FORK] END"),
                    ]),
                    ForkOutcome::Malformed(msg) => Some(vec![
                        CsaLine::new(format!("##[FORK] ERROR {} {msg}", buoy_name.as_str())),
                        CsaLine::new("##[FORK] END"),
                    ]),
                    ForkOutcome::Derived(derived) => match state
                        .buoy_storage
                        .store(&buoy_name, Vec::new(), 1, Some(derived.initial_sfen.clone()))
                        .await
                    {
                        Ok(()) => Some(vec![
                            CsaLine::new(format!(
                                "##[FORK] OK {} {}",
                                buoy_name.as_str(),
                                derived.applied_moves
                            )),
                            CsaLine::new("##[FORK] END"),
                        ]),
                        Err(e) => Some(vec![
                            CsaLine::new(format!("##[FORK] ERROR {} {e}", buoy_name.as_str())),
                            CsaLine::new("##[FORK] END"),
                        ]),
                    },
                }
            }
            _ => None,
        };
        let Some(lines) = replies else {
            // 未サポートの x1 コマンド / 対局中コマンドは切断扱い（未配線の
            // x1 拡張以外はここへ落とす）。
            let mut pool = state.waiting.lock().await;
            let _removed = pool.remove_by_handle(&game_name, &handle);
            break 'outer WaiterOutcome::DisconnectedFromPool;
        };
        // `%%HELP` / `%%WHO` / `%%LIST` / `%%SHOW` は末尾の `##[<TAG>] END` 行で
        // 1 応答として完結する contract。途中でマッチ要求が来ても送出を中断
        // してはいけない（client が END を待ったまま詰まる）ので、1 応答は
        // 必ず全行送りきってからループ先頭 `tokio::select!` でマッチ確定
        // 待ちに戻る。マッチは 1 応答分の遅れ（数行の write 時間）だけ
        // 引き延ばされるが、相互の framing を壊さないためのトレードオフ。
        //
        // ただし、応答を読まずに詰まった x1 client を無期限に抱え込むと、
        // 対局相手の handoff（`resp_rx.await`）が永久に停止してマッチメイキング
        // 全体が止まる。そのため 1 行ごとに `x1_reply_write_timeout` を上限として
        // 適用し、超過・失敗いずれも切断扱いで pool から除去する。
        let write_timeout = state.config.x1_reply_write_timeout;
        let mut stall_cause: Option<&'static str> = None;
        for out in lines {
            match tokio::time::timeout(write_timeout, transport.send_line(&out)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    stall_cause = Some("io");
                    break;
                }
                Err(_) => {
                    stall_cause = Some("timeout");
                    break;
                }
            }
        }
        if let Some(cause) = stall_cause {
            // x1 waiter の応答 write が止まった際は、運用側が原因を分類できるよう
            // cause を必ずログに残す（timeout = client が読まずに詰まり、
            // io = peer 切断・I/O エラー）。マッチメイキング全体の停止を防ぐため
            // この経路で常に pool から除去・League logout する。
            tracing::info!(
                cause,
                handle = %handle,
                game_name = %game_name,
                "x1 waiter write stalled; dropping session"
            );
            let mut pool = state.waiting.lock().await;
            let _removed = pool.remove_by_handle(&game_name, &handle);
            break 'outer WaiterOutcome::DisconnectedFromPool;
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

/// buoy 解決結果。通常対局 / buoy 起点 / 枯渇の 3 分岐を区別する。
enum MatchInitialPosition {
    /// buoy 未設定。グローバル既定値 (`ServerConfig::initial_sfen`) を使う。
    Default(Option<String>),
    /// buoy が有効で、今回の対局用に消費済み。
    Reserved(String),
    /// buoy は存在するが残数 0。対局を成立させない。
    Exhausted,
}

/// `%%FORK` 派生の結果。
struct ForkDerivation {
    initial_sfen: String,
    applied_moves: u32,
}

/// `%%FORK` の派生処理の結末。malformed は接続を切らずに x1 応答で
/// `##[FORK] ERROR ...` に落とすため、Result の Err としては扱わない。
enum ForkOutcome {
    /// 元棋譜が存在しない。
    NotFound,
    /// 元棋譜は見つかったが CSA として壊れている／`nth_move` が範囲外。
    Malformed(String),
    /// 派生成功。
    Derived(ForkDerivation),
}

/// `%%MONITOR2ON` の TOCTOU 再確認用ヘルパ。`subscribe` 完了後に game_id が
/// まだ `GameRegistry` に存在するかを確認する。
///
/// `subscribe` の前後は drive 側の `unregister + clear_room` に対して非原子的で、
/// subscribe 完了時点でゲームが既に終局している可能性がある。その場合 stale
/// なエントリを broadcaster に残さないよう、呼び出し側で drop + prune して
/// NOT_FOUND を返す (Codex review PR #469 3rd round P2)。
async fn subscribe_still_registered<R, K, P>(state: &SharedState<R, K, P>, game_id: &GameId) -> bool
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let games = state.games.lock().await;
    games.get(game_id).is_some()
}

/// 待機プールから相手を拾った後に、その対局で使う開始局面を確定する。
///
/// buoy があれば残数を 1 消費してその開始局面を返し、無ければグローバル既定値を返す。
/// 残数 0 の buoy は対局を成立させない。
async fn reserve_match_initial_position<R, K, P>(
    state: &SharedState<R, K, P>,
    game_name: &GameName,
) -> Result<MatchInitialPosition, ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let Some(buoy) = state
        .buoy_storage
        .reserve_for_match(game_name)
        .await
        .map_err(ServerError::Storage)?
    else {
        return Ok(MatchInitialPosition::Default(state.config.initial_sfen.clone()));
    };
    if buoy.remaining == 0 {
        return Ok(MatchInitialPosition::Exhausted);
    }
    let initial_sfen = match buoy.initial_sfen {
        Some(sfen) => sfen,
        None => match initial_sfen_from_csa_moves(&buoy.moves) {
            Ok(sfen) => sfen,
            Err(e) => {
                // legacy buoy (initial_sfen 無し、moves からの導出) で moves が
                // 不正な場合、`reserve_for_match` で既に消費した 1 回分を
                // 巻き戻す。そうしないと不正 buoy が静かに burn し続ける
                // (Copilot レビュー指摘)。
                if let Err(rollback_err) = state.buoy_storage.release_reservation(game_name).await {
                    tracing::error!(
                        %game_name,
                        error = %rollback_err,
                        "failed to rollback buoy reservation"
                    );
                }
                return Err(ServerError::Protocol(ProtocolError::Malformed(format!(
                    "buoy {game_name}: {e}"
                ))));
            }
        },
    };
    Ok(MatchInitialPosition::Reserved(initial_sfen))
}

/// `%%FORK` の入力を既存棋譜から SFEN に落とす。
///
/// 元棋譜が見つからない／壊れている／`nth_move` が範囲外のケースは `Err` では
/// なく [`ForkOutcome`] の `NotFound` / `Malformed` バリアントで返す。waiter
/// ループ側は x1 応答 `##[FORK] NOT_FOUND` / `##[FORK] ERROR ...` に落として
/// 接続を維持し、graceful degradation にする。`Err` は storage I/O 失敗など
/// 本当に復旧不能な経路にだけ残す。
async fn derive_fork_from_source_kifu<R, K, P>(
    state: &SharedState<R, K, P>,
    source_game: &GameId,
    nth_move: Option<u32>,
) -> Result<ForkOutcome, ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let Some(csa_v2_text) =
        state.kifu_storage.load(source_game).await.map_err(ServerError::Storage)?
    else {
        return Ok(ForkOutcome::NotFound);
    };
    match fork_initial_sfen_from_kifu(&csa_v2_text, nth_move) {
        Ok((initial_sfen, applied_moves)) => Ok(ForkOutcome::Derived(ForkDerivation {
            initial_sfen,
            applied_moves,
        })),
        Err(e) => Ok(ForkOutcome::Malformed(format!("%%FORK {}: {e}", source_game.as_str()))),
    }
}

fn default_fork_buoy_name(source_game: &GameId, nth_move: Option<u32>) -> GameName {
    let suffix = nth_move.map_or_else(|| "final".to_owned(), |n| n.to_string());
    GameName::new(format!("{}-fork-{}", source_game.as_str(), suffix))
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
    game_name: GameName,
    match_initial_sfen: Option<String>,
    opp_completion_tx: oneshot::Sender<()>,
) -> Result<(), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    debug_assert_eq!(opp_color, self_color.opposite());

    // `drive_game` 在籍をカウントする RAII ガード。graceful shutdown の完了
    // 判定で使うため、`persist_kifu` を含む epilogue 全体が終わるまで生存
    // させる必要がある。Err 早期 return / panic でも確実に decrement + notify
    // されるように `Drop` で解放する。
    struct DriveGuard<'a> {
        counter: &'a AtomicUsize,
        notify: &'a Notify,
    }
    impl Drop for DriveGuard<'_> {
        fn drop(&mut self) {
            self.counter.fetch_sub(1, Ordering::Release);
            self.notify.notify_waiters();
        }
    }
    state.active_drive_tasks.fetch_add(1, Ordering::Release);
    let _drive_guard = DriveGuard {
        counter: &state.active_drive_tasks,
        notify: &state.active_games,
    };

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
    // 確定した game_id を現在の tracing span に追加し、以後この対局タスク内で
    // 発行されるイベントに `game_id = "<id>"` を伝播させる。conn span の `id`
    // フィールドと併せて、接続単位 + 対局単位の二段相関 ID を CI ログから
    // 一意に追えるようにする。
    tracing::Span::current().record("game_id", tracing::field::display(&game_id));

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
    // GameRegistry の register は `drive_game_inner` 内で両者 AGREE 成立を確認
    // してから入れる（AGREE 待ち中に REJECT / %CHUDAN / 切断で不成立になった
    // 対局を `%%LIST` / `%%SHOW` に出さないため）。unregister は本関数 epilogue で
    // 無条件に呼ぶ（未登録 game_id への unregister は no-op）。
    let inner = drive_game_inner(
        state.as_ref(),
        &game_id,
        matched.clone(),
        game_name.clone(),
        match_initial_sfen.clone(),
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
    {
        let mut games = state.games.lock().await;
        games.unregister(&game_id);
    }
    state.broadcaster.clear_room(&RoomId::new(game_id.as_str())).await;
    // 待機タスクに完了通知（これで先着側のタスクが抜ける）。
    let _ = opp_completion_tx.send(());
    // `active_drive_tasks` の decrement + `active_games.notify_waiters()` は
    // `_drive_guard` の Drop で行う。ここで明示的に呼ぶと二重通知になり、
    // Err 早期 return 経路との挙動差も生まれるので guard に一任する。
    inner
}

/// `confirm_match` 後の主処理。Game_Summary → AGREE → 対局 → 棋譜永続化までを行う。
/// 本関数は League/Pool の後始末を行わない（呼び出し側 `drive_game` が必ず実行する）。
async fn drive_game_inner<R, K, P>(
    state: &SharedState<R, K, P>,
    game_id: &GameId,
    matched: MatchedPair,
    game_name: GameName,
    match_initial_sfen: Option<String>,
    black_transport: &mut TcpTransport,
    white_transport: &mut TcpTransport,
) -> Result<(), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    // Game_Summary を両対局者に送信。
    let clock = state.config.clock.build_clock();
    let time_section = state.config.clock.format_time_section();
    // `initial_sfen` が設定されていればそれから派生、無ければ平手固定のブロックを使う。
    // GameRoom / Game_Summary / 棋譜 の三点一致契約 (GameRoomConfig::initial_sfen の
    // doc を参照) を満たすため、同じ SFEN を複数入口で再利用する。
    let (position_section, to_move) = match &match_initial_sfen {
        Some(sfen) => {
            let section = position_section_from_sfen(sfen).map_err(|e| {
                ServerError::Protocol(ProtocolError::Malformed(format!("initial_sfen: {e}")))
            })?;
            let side = side_to_move_from_sfen(sfen).map_err(|e| {
                ServerError::Protocol(ProtocolError::Malformed(format!("initial_sfen: {e}")))
            })?;
            (section, side)
        }
        None => (standard_initial_position_block(), Color::Black),
    };
    let summary = GameSummaryBuilder {
        game_id: game_id.clone(),
        black: matched.black.clone(),
        white: matched.white.clone(),
        time_section,
        position_section,
        rematch_on_draw: false,
        to_move,
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

    // `GameRoom` を構築して内部 AGREE を 2 回入れ、`START` 配信まで済ませる。
    // 先に dispatch を通し、成功後に初めて League と GameRegistry を更新する。
    // こうすることで START 配信が遅延・詰まり・失敗している間は「League は
    // AgreeWaiting のまま、GameRegistry も空」の一貫した状態を保てる
    //（WHO が `playing:<game_id>` を返すのに LIST / SHOW には出ない、という
    // 不整合を防ぐ）。
    let start_time = chrono::Utc::now();
    let (mut room, game_start_instant) = initialize_game_and_dispatch_start(
        state,
        game_id,
        &matched,
        clock,
        match_initial_sfen.clone(),
        black_transport,
        white_transport,
    )
    .await?;

    // `START` 配信成功を確認してから、League → `InGame` 遷移と GameRegistry
    // 登録を連続で行う。2 つの共有状態更新は micro 秒スケールで連続するので、
    // `%%WHO` と `%%LIST` / `%%SHOW` が同じ「対局開始」状態を観測する。
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
    // `started_at_iso` は棋譜の `start_time` と同じ瞬間を表すべきなので、
    // 別途 `Utc::now()` を取らず `start_time` から派生させる（`%%SHOW` の
    // `started_at` フィールドと棋譜ヘッダの開始時刻が常に一致する）。
    let started_at_iso = start_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    {
        let mut games = state.games.lock().await;
        games.register(GameListing {
            game_id: game_id.clone(),
            black: matched.black.clone(),
            white: matched.white.clone(),
            game_name,
            started_at: started_at_iso,
        });
    }

    // 指し手と消費時間を記録しつつ終局まで駆動する。
    let result_moves = run_game_loop_and_record(
        state,
        game_id,
        &mut room,
        game_start_instant,
        black_transport,
        white_transport,
    )
    .await;
    let end_time = chrono::Utc::now();

    // 終局（正常 / I/O 失敗いずれも）を観測したら、League の状態遷移と
    // GameRegistry の unregister を persist_kifu より先に行う。`%%WHO` は
    // `League` を、`%%LIST` / `%%SHOW` は `GameRegistry` を読むので、両者を
    // 同じタイミングで「対局終了」側に寄せることで、遅いストレージを使う
    // 運用でも WHO と LIST / SHOW の一貫性が保たれる（`persist_kifu` 中に
    // `%%WHO` が `playing:<game_id>` を返す一方で `%%LIST` では既に消えて
    // いる、という不整合を防ぐ）。`drive_game` epilogue の end_game / logout /
    // unregister はいずれも idempotent なので、ここで先行してもダブルコール
    // で破綻しない。
    //
    // **shutdown 判定との関係**: graceful shutdown の「対局完了待ち」は
    // `GameRegistry` 件数ではなく `SharedState::active_drive_tasks`
    // (AtomicUsize) を真実源とする。`drive_game` の RAII guard が epilogue の
    // 最後 (persist_kifu 完了後) で decrement するため、ここでの `unregister`
    // を前倒ししても shutdown 判定は 0 に落ちない。逆に言うと、将来
    // `active_game_count()` の参照先をうかつに `GameRegistry` に戻すと
    // persist_kifu 中の棋譜消失 race が再発するので注意。
    {
        let mut league = state.league.lock().await;
        let _ = league.end_game(&matched);
    }
    {
        let mut games = state.games.lock().await;
        games.unregister(game_id);
    }

    // run_game_loop の失敗はそのまま早期 return する（persist_kifu は行わない）。
    let (result, moves) = result_moves?;

    // 棋譜 + 00LIST 永続化。
    persist_kifu(
        state,
        game_id,
        &matched,
        match_initial_sfen.as_deref(),
        start_time,
        end_time,
        &moves,
        &result,
    )
    .await?;
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

/// `GameRoom` を初期化し、内部 AGREE 2 回 + 最初の `START` 配信までを行う。
///
/// 成功すると「クライアントが対局開始を受け取れた」ことが保証されるので、
/// 呼び出し側は続けて `GameRegistry::register` してから `run_game_loop_and_record`
/// を呼ぶ流れに乗せる。`dispatch` が送信失敗した場合は `ServerError::Transport`
/// で早期 return し、GameRegistry には入れない（幽霊対局を防ぐ）。
async fn initialize_game_and_dispatch_start<R, K, P>(
    state: &SharedState<R, K, P>,
    game_id: &GameId,
    matched: &MatchedPair,
    clock: Box<dyn rshogi_csa_server::TimeClock>,
    match_initial_sfen: Option<String>,
    black: &mut TcpTransport,
    white: &mut TcpTransport,
) -> Result<(GameRoom, tokio::time::Instant), ServerError>
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
        initial_sfen: match_initial_sfen,
    };
    let mut room = GameRoom::new(cfg, clock)?;

    let start_instant = tokio::time::Instant::now();
    let now_ms =
        || tokio::time::Instant::now().saturating_duration_since(start_instant).as_millis() as u64;

    // 対局開始を双方に通知するため、内部的に AGREE を 2 回入れてから Playing に進める。
    // 外部クライアントからの AGREE は `wait_both_agree` で受信済みなので、ここでは
    // GameRoom の内部状態だけを進める。`START` 行は 2 回目の AGREE 処理で
    // broadcasts に積まれる。
    room.handle_line(Color::Black, &CsaLine::new("AGREE"), now_ms())?;
    let r = room.handle_line(Color::White, &CsaLine::new("AGREE"), now_ms())?;
    dispatch(&r.broadcasts, black, white, &state.broadcaster, &RoomId::new(game_id.as_str()))
        .await?;

    Ok((room, start_instant))
}

/// 既に `START` 配信済みの `GameRoom` を受け取り、終局まで駆動する。
///
/// `run_room` を直接使うと消費秒数を取り出せないため、ここでは `GameRoom` を直接駆動
/// して手番イベントから `,T<sec>` を解析し `KifuMove` を収集する。
async fn run_game_loop_and_record<R, K, P>(
    state: &SharedState<R, K, P>,
    game_id: &GameId,
    room: &mut GameRoom,
    start_instant: tokio::time::Instant,
    black: &mut TcpTransport,
    white: &mut TcpTransport,
) -> Result<(GameResult, Vec<KifuMove>), ServerError>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let now_ms =
        || tokio::time::Instant::now().saturating_duration_since(start_instant).as_millis() as u64;
    let mut recorded_moves: Vec<KifuMove> = Vec::new();

    loop {
        let status = room.status().clone();
        if let rshogi_csa_server::GameStatus::Finished(result) = status {
            return Ok((result, recorded_moves));
        }
        let deadline = compute_timeup_deadline(room);
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
    initial_sfen: Option<&str>,
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
    // initial_sfen が設定されていれば棋譜の `initial_position` も同じ SFEN から派生。
    // 設定されていない (= 平手) 場合は既存の CSA shorthand `PI\n+\n` を保つ。
    // 長期的には常に `BEGIN Position` 形式に統一しても良いが、shogi-server 互換
    // バッチへの影響を避けるため hirate のみ現行踏襲 (deferral)。
    let initial_position = match initial_sfen {
        Some(sfen) => position_section_from_sfen(sfen).map_err(|e| {
            ServerError::Protocol(ProtocolError::Malformed(format!("initial_sfen: {e}")))
        })?,
        None => "PI\n+\n".to_owned(),
    };
    let record = KifuRecord {
        game_id: game_id.clone(),
        black: matched.black.clone(),
        white: matched.white.clone(),
        start_time: start_time.format("%Y/%m/%d %H:%M:%S").to_string(),
        end_time: end_time.format("%Y/%m/%d %H:%M:%S").to_string(),
        event: "rshogi-csa-server-tcp".to_owned(),
        time_section: state.config.clock.format_time_section(),
        initial_position,
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
    let buoy_storage = rshogi_csa_server::FileBuoyStorage::new(config.kifu_topdir.clone());
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
        games: Mutex::new(GameRegistry::new()),
        active_drive_tasks: AtomicUsize::new(0),
        active_games: Notify::new(),
        game_counter: Mutex::new(0),
        started_at: chrono::Utc::now(),
        buoy_storage,
        shutdown: GracefulShutdown::new(),
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

    /// 既定構成は Floodgate 系機能を要求していないため、`allow_floodgate_features=false`
    /// のままでも `prepare_runtime` が成功する。これが崩れると通常起動経路が
    /// 全停止するため、契約として固定する。
    #[test]
    fn prepare_runtime_passes_for_default_config_without_floodgate_optin() {
        let cfg = ServerConfig::sensible_defaults();
        assert!(!cfg.allow_floodgate_features);
        prepare_runtime(&cfg).expect("default config must start without floodgate opt-in");
    }

    /// 将来 Floodgate 機能が `floodgate_intent_from_config` に配線された後、
    /// `allow_floodgate_features=false` のままで起動を試みると fail-fast する
    /// 契約を直接検証する。helper 経路ではなく gate 関数を直接テストするのは、
    /// 現状 `floodgate_intent_from_config` が常に `Default` を返すため。
    #[test]
    fn floodgate_gate_rejects_intent_when_optin_is_off() {
        let intent = FloodgateFeatureIntent {
            enable_scheduler: true,
            ..FloodgateFeatureIntent::default()
        };
        let err = validate_floodgate_feature_gate(false, intent).unwrap_err();
        assert!(err.contains("scheduler"), "error must list requested feature: {err}");
    }
}
