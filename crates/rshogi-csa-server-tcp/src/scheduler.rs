//! Floodgate スケジューラ実行系（TCP frontend 用）。
//!
//! コア crate `rshogi_csa_server::scheduler` で定義された値型 / trait
//! ([`FloodgateSchedule`] / [`FloodgateTimer`]) を実装し、tokio current_thread
//! ランタイム上でスケジュール定刻にマッチメイクを発火する。
//!
//! # ライフサイクル
//!
//! [`run_schedules`] が `state.config.floodgate_schedules` の各エントリを
//! 独立した tokio task に配って起動する。各タスクは:
//!
//! 1. `next_fire_after(now)` で次回発火 UTC 時刻を算出
//! 2. `FloodgateTimer::wait_until` または shutdown シグナルを `tokio::select!`
//!    で並列待機
//! 3. shutdown が立った場合は即座に終了。発火タイミングが先に来たら
//!    [`fire_schedule`] を呼んで 1 周分マッチメイクし、ループに戻る
//!
//! # `fire_schedule` のフロー
//!
//! 1. `WaitingPool::drain_for_game_name` で当該 `game_name` の全 slot を取得
//! 2. `PairingLogic::try_pair` で `Vec<MatchedPair>` を計算
//! 3. ペア化された slot は両 waiter に [`MatchRequest`] を送って transport を
//!    吸い上げ、`drive_game` を `spawn_local` で起動
//! 4. ペア化されなかった slot は WaitingPool に再 push（次回まで待機）
//!
//! # 既知の制約（後続タスクで対応）
//!
//! - **per-schedule clock**: 現状は `state.config.clock`（global）を使う。
//!   スケジュール毎に異なる時計（`floodgate-600-10` と `floodgate-180-3` 等）
//!   をサポートするには `drive_game` のシグネチャに `ClockSpec` 引数を足して
//!   呼び出し側で上書きする必要があり、本タスクの範囲外として持ち越し。
//! - **buoy / 駒落ち**: スケジューラ起動の対局は常に平手（`initial_sfen = None`）。
//!   駒落ちサポートはタスク 15.4 で対応する。

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rshogi_csa_server::matching::pairing::{DirectMatchStrategy, PairingLogic};
use rshogi_csa_server::scheduler::{FloodgateSchedule, FloodgateTimer};
use rshogi_csa_server::types::{Color, GameName, PlayerName};
use rshogi_csa_server::{KifuStorage, PairingCandidate, RateStorage};
use tokio::sync::oneshot;

use crate::server::{MatchRequest, PasswordStore, SharedState, WaitingSlot, drive_game};
use crate::transport::TcpTransport;
use rshogi_csa_server::ClientTransport;
use rshogi_csa_server::types::CsaLine;

/// Floodgate 定刻発火で waiter から transport を回収する際の最大待機時間。
///
/// 通常 waiter は `MatchRequest` を受信した select! 枝で即座に
/// `transport_responder.send(transport)` を呼ぶため、この timeout は ms 単位の
/// 応答前提のセーフティネットとして 5 秒に置く。waiter 側が deadlock した場合
/// （別 select 枝で stall 等）でも本 timeout で scheduler ループが永久ブロック
/// するのを防ぐ。timeout 経過後の transport は abort 扱いで logout_pair の
/// 経路に乗せる。
const TRANSPORT_HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);

/// `transport_responder` の oneshot 受信に失敗した理由を区別する。
///
/// scheduler の handoff 経路で waiter が transport を引き渡せなかった場合の
/// 内訳をログに残すため、`SenderDropped`（waiter loop が responder を drop）と
/// `TimedOut`（[`TRANSPORT_HANDOFF_TIMEOUT`] 経過）を分ける。両者とも abort
/// 経路に流すが、運用ログから「waiter が応答を返せない deadlock 状態」を
/// 即特定できるようにする。
#[derive(Debug, Clone, Copy)]
enum TransportHandoffError {
    /// waiter が transport を送る前に responder の sender 側が drop された
    /// （waiter loop が exit / channel が壊れた等）。
    SenderDropped,
    /// `TRANSPORT_HANDOFF_TIMEOUT` を経過しても waiter が transport を送らなかった。
    TimedOut,
}

/// `transport_responder` の受信に [`TRANSPORT_HANDOFF_TIMEOUT`] のタイムアウトを
/// 被せる。timeout か sender drop のどちらでも `Err` で abort 経路に流す。
async fn recv_transport_with_timeout(
    rx: oneshot::Receiver<TcpTransport>,
) -> Result<TcpTransport, TransportHandoffError> {
    match tokio::time::timeout(TRANSPORT_HANDOFF_TIMEOUT, rx).await {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(_)) => Err(TransportHandoffError::SenderDropped),
        Err(_) => Err(TransportHandoffError::TimedOut),
    }
}

/// `tokio::time::sleep_until` ベースの `FloodgateTimer` 実装。
///
/// `current_thread` ランタイム上で動き、deadline までスリープする。deadline が
/// 既に過去であれば即座に return する（spurious tick 等の安全側挙動）。
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioFloodgateTimer;

impl FloodgateTimer for TokioFloodgateTimer {
    async fn wait_until(&self, deadline: DateTime<Utc>) {
        let now = Utc::now();
        let dur = (deadline - now).to_std().unwrap_or(std::time::Duration::ZERO);
        if dur.is_zero() {
            return;
        }
        tokio::time::sleep(dur).await;
    }
}

/// `pairing_strategy` 文字列からペアリング戦略インスタンスを構築する。
///
/// `"direct"` のみ本タスクで配線する。`"least_diff"` 等の Floodgate 系戦略は
/// 別タスクで `Box<dyn PairingLogic>` 化するクロージャを足す形で拡張する。
/// 未知の名前は起動時に `Err` で fail-fast する（`run_schedules` 経由で）。
pub(crate) fn build_strategy(name: &str) -> Result<Box<dyn PairingLogic>, String> {
    match name {
        "direct" => Ok(Box::new(DirectMatchStrategy)),
        other => Err(format!("unknown pairing_strategy {other:?}; supported: \"direct\"")),
    }
}

/// 全 [`FloodgateSchedule`] をそれぞれ独立した task に乗せて起動する。
///
/// 戻り値は spawn された各 task の `JoinHandle<()>`。`run_server` 起動と並行に
/// `tokio::task::spawn_local` で呼ばれることを想定する。各 task は内部で
/// `state.shutdown` を監視し、shutdown 時に自動終了する。main 側は join handle
/// を保持し、shutdown 後の `await` で終了確認する。
///
/// 戦略構築は task spawn より前にまとめて行い、未知 strategy 名は起動段階で
/// `Err` を返す（run loop に入ってから初めて detected すると、無音でスケジュール
/// が動かない事故になりうる）。
pub fn run_schedules<R, K, P>(
    state: Rc<SharedState<R, K, P>>,
) -> Result<Vec<tokio::task::JoinHandle<()>>, String>
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let schedules = state.config().floodgate_schedules.clone();
    let mut handles = Vec::with_capacity(schedules.len());
    for schedule in schedules {
        let strategy = build_strategy(&schedule.pairing_strategy)?;
        let s = state.clone();
        let h = tokio::task::spawn_local(async move {
            run_one_schedule(s, schedule, strategy, TokioFloodgateTimer).await;
        });
        handles.push(h);
    }
    Ok(handles)
}

async fn run_one_schedule<R, K, P, T>(
    state: Rc<SharedState<R, K, P>>,
    schedule: FloodgateSchedule,
    strategy: Box<dyn PairingLogic>,
    timer: T,
) where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
    T: FloodgateTimer + 'static,
{
    // release ビルドでは scheduler ループ全体を `catch_unwind` で囲み、`fire_schedule`
    // 内で起きた panic がこのスケジュールタスクを静かに殺すのを防ぐ。catch 後は
    // `tracing::error!` で記録してタスクは終了させる（再 spawn は YAGNI）。debug
    // ビルドでは透過させて契約違反を顕在化させる（CLAUDE.md 「契約違反は panic」
    // 方針）。`AssertUnwindSafe` の根拠は `run_connection_isolated` と同様で、
    // SharedState の可変フィールドは Mutex / Atomic / Notify で構成されており、
    // `drive_game` 側の `DriveGuard` Drop で in-flight 状態は巻き戻る。
    #[cfg(debug_assertions)]
    {
        run_one_schedule_loop(state, schedule, strategy, timer).await;
    }
    #[cfg(not(debug_assertions))]
    {
        use futures_util::FutureExt;
        let game_name_for_log = schedule.game_name.clone();
        let fut =
            std::panic::AssertUnwindSafe(run_one_schedule_loop(state, schedule, strategy, timer));
        match fut.catch_unwind().await {
            Ok(()) => {}
            Err(payload) => {
                tracing::error!(
                    game_name = %game_name_for_log,
                    panic_payload = %crate::server::panic_payload_to_string(payload.as_ref()),
                    "scheduler task panicked; isolated to this schedule"
                );
            }
        }
    }
}

async fn run_one_schedule_loop<R, K, P, T>(
    state: Rc<SharedState<R, K, P>>,
    schedule: FloodgateSchedule,
    strategy: Box<dyn PairingLogic>,
    timer: T,
) where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
    T: FloodgateTimer + 'static,
{
    let game_name_for_log = schedule.game_name.clone();
    loop {
        if state.shutdown.is_triggered() {
            tracing::info!(
                game_name = %game_name_for_log,
                "scheduler task exiting (shutdown triggered)"
            );
            return;
        }
        let now = Utc::now();
        let next_fire = schedule.next_fire_after(now);
        tracing::info!(
            game_name = %game_name_for_log,
            strategy = strategy.name(),
            next_fire = %next_fire,
            "scheduler waiting for next fire"
        );
        tokio::select! {
            _ = timer.wait_until(next_fire) => {}
            _ = state.shutdown.wait() => {
                tracing::info!(
                    game_name = %game_name_for_log,
                    "scheduler task exiting (shutdown signal during wait)"
                );
                return;
            }
        }
        // shutdown が wait_until 完了と同時に立っていた場合は fire しない。
        if state.shutdown.is_triggered() {
            return;
        }
        fire_schedule(state.clone(), &schedule, strategy.as_ref()).await;
    }
}

/// 1 回分のマッチメイク発火: 待機 slot を取得 → ペア化 → drive_game を spawn。
///
/// 副作用は state.waiting / state.league（drive_game 内）/ tokio::task::spawn_local
/// に閉じる。ペアリング戦略の純関数部分とテスト容易性のため、戦略は呼び出し側が
/// `&dyn PairingLogic` で渡す（`run_schedules` 経路では `Box<dyn ...>` を借りる）。
pub(crate) async fn fire_schedule<R, K, P>(
    state: Rc<SharedState<R, K, P>>,
    schedule: &FloodgateSchedule,
    strategy: &dyn PairingLogic,
) where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let game_name = schedule.game_name();

    // 1. WaitingPool から当該 game_name の全 slot を抜き取る。
    let drained: Vec<WaitingSlot> = {
        let mut pool = state.waiting.lock().await;
        pool.drain_for_game_name(&game_name)
    };
    if drained.is_empty() {
        tracing::info!(
            game_name = %schedule.game_name,
            "scheduler fired but no waiters were in the pool"
        );
        return;
    }

    // 2. PairingCandidate に変換して戦略を回す。
    let candidates: Vec<PairingCandidate> = drained
        .iter()
        .map(|s| PairingCandidate {
            name: PlayerName::new(&s.handle),
            preferred_color: Some(s.color),
        })
        .collect();
    // 注: 現在配線されている戦略 [`DirectMatchStrategy`] は最初に見つかった
    // 1 ペアだけを返す（複数ペア成立は未対応）。`pairs.len()` を引数に下流処理
    // を組んでいるが、`"direct"` 経由では最大 1 ペアに収束する。複数ペア対応
    // は Floodgate の `"least_diff"` 戦略を入れるタスクで multi-pair 対応戦略を
    // 追加して切り替える想定。
    let pairs = strategy.try_pair(&candidates);
    tracing::info!(
        game_name = %schedule.game_name,
        waiters = drained.len(),
        pairs = pairs.len(),
        strategy = strategy.name(),
        "scheduler fired"
    );

    // 3. ペア成立した slot と未成立 slot に分割。
    let mut by_handle: HashMap<String, WaitingSlot> =
        drained.into_iter().map(|s| (s.handle.clone(), s)).collect();
    let mut to_drive: Vec<(WaitingSlot, WaitingSlot)> = Vec::with_capacity(pairs.len());
    for pair in &pairs {
        let black = by_handle.remove(pair.black.as_str());
        let white = by_handle.remove(pair.white.as_str());
        match (black, white) {
            (Some(b), Some(w)) => to_drive.push((b, w)),
            // どちらか取り出せなかったケース: 戦略が不整合な MatchedPair を返した
            // 異常系。戦略実装の不変条件違反として log のみ残し、両 slot は
            // 残置（後で leftover として再キュー）。
            (b, w) => {
                if let Some(slot) = b {
                    by_handle.insert(slot.handle.clone(), slot);
                }
                if let Some(slot) = w {
                    by_handle.insert(slot.handle.clone(), slot);
                }
                tracing::warn!(
                    game_name = %schedule.game_name,
                    pair_black = %pair.black.as_str(),
                    pair_white = %pair.white.as_str(),
                    "pairing strategy returned a MatchedPair with handles not in candidate set"
                );
            }
        }
    }

    // 4. 不成立 slot を WaitingPool に再 push（決定論的順序）。
    {
        let mut pool = state.waiting.lock().await;
        let mut leftover: Vec<WaitingSlot> = by_handle.into_values().collect();
        leftover.sort_by(|a, b| a.handle.cmp(&b.handle));
        for slot in leftover {
            pool.push(game_name.clone(), slot);
        }
    }

    // 5. 成立ペアごとに transport を吸い上げて drive_game を spawn。
    for (black, white) in to_drive {
        spawn_scheduled_drive(state.clone(), game_name.clone(), black, white).await;
    }
}

/// 1 ペア分の transport handoff と drive_game spawn。
///
/// 両 waiter に [`MatchRequest`] を送って transport を回収し、`drive_game` を
/// `spawn_local` で起動する。`drive_game` は片方の waiter（black）の done_tx を
/// 消費して終局通知するので、もう片方（white）の done_tx は本関数が drive 完了
/// 後に手動で signal する。
///
/// 異常系（`MatchRequest` 配送失敗 / transport recv 失敗）では:
/// - 既に `MatchRequest` が届いた側 waiter は `transport_responder.send` を
///   呼んで transport を引き渡そうとしているので、本関数が responder_rx を
///   `await` して transport を回収し `##[ERROR]` 通知してから close する
///   （片側瞬断 partial handoff 経路でも生存側を無通知切断しない）
/// - **両 player を `League` から logout**（`drain_for_game_name` で WaitingPool
///   からは除去済みなので、League を生で残すと再 LOGIN が `AlreadyLoggedIn` で
///   弾かれる leak になるため）
/// - drop により残った oneshot は recv Err になり、surviving waiter は
///   `WaiterOutcome::Completed` 経路で抜ける（drive_game に到達していないため
///   logout は本関数が代行する）
async fn spawn_scheduled_drive<R, K, P>(
    state: Rc<SharedState<R, K, P>>,
    game_name: GameName,
    black: WaitingSlot,
    white: WaitingSlot,
) where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let (b_resp_tx, b_resp_rx) = oneshot::channel::<TcpTransport>();
    let (b_done_tx, b_done_rx) = oneshot::channel::<()>();
    let (w_resp_tx, w_resp_rx) = oneshot::channel::<TcpTransport>();
    let (w_done_tx, w_done_rx) = oneshot::channel::<()>();

    let b_req = MatchRequest {
        transport_responder: b_resp_tx,
        completion_rx: b_done_rx,
    };
    let w_req = MatchRequest {
        transport_responder: w_resp_tx,
        completion_rx: w_done_rx,
    };

    let black_handle = black.handle.clone();
    let white_handle = white.handle.clone();
    let b_handoff_ok = black.match_request_tx.send(b_req).is_ok();
    let w_handoff_ok = white.match_request_tx.send(w_req).is_ok();
    if !b_handoff_ok || !w_handoff_ok {
        tracing::warn!(
            game_name = %game_name.as_str(),
            black = %black_handle,
            white = %white_handle,
            black_alive = b_handoff_ok,
            white_alive = w_handoff_ok,
            "scheduled match handoff failed (waiter disconnected before handoff)"
        );
        // 片側だけ MatchRequest が届いたケースでは、生存側 waiter は
        // `transport_responder.send(transport)` を呼んで transport を引き渡そうと
        // している（[`run_waiter`] 設計）。本関数が即 return すると responder_rx
        // が drop されて waiter 側が `MatchHandoffFailed` で無通知切断する。
        // 既存の `(Ok, Err) | (Err, Ok)` recv 経路と同じ扱いに統一し、生存側
        // transport を吸い上げて `##[ERROR]` 通知を送ってから drop することで
        // 片側瞬断時でも健全側 player に切断理由を残す。
        // `recv_transport_with_timeout` で `TRANSPORT_HANDOFF_TIMEOUT` を被せ、
        // waiter 側が応答を返せない deadlock 状態でも本経路は永久ブロックしない。
        if b_handoff_ok && let Ok(mut surviving) = recv_transport_with_timeout(b_resp_rx).await {
            notify_aborted_match(&mut surviving, &game_name).await;
        }
        if w_handoff_ok && let Ok(mut surviving) = recv_transport_with_timeout(w_resp_rx).await {
            notify_aborted_match(&mut surviving, &game_name).await;
        }
        // WaitingPool からは drain 済みで、drive_game にも到達していないので
        // League エントリは本関数が責任を持って除去する。
        logout_pair(&state, &black_handle, &white_handle).await;
        return;
    }

    // 両 waiter から transport を吸い上げる。`recv_transport_with_timeout` で
    // [`TRANSPORT_HANDOFF_TIMEOUT`] を被せ、waiter 直後切断（SenderDropped）と
    // waiter deadlock（TimedOut）の両方を `Err` として abort 経路に流す。
    // recv 失敗時は:
    // - 確定した側の transport には `##[ERROR]` 通知を送って drop（無音切断回避）
    // - 双方とも League から logout（drive_game に到達しないため孤児化防止）
    let b_recv = recv_transport_with_timeout(b_resp_rx).await;
    let w_recv = recv_transport_with_timeout(w_resp_rx).await;
    let (b_transport, w_transport) = match (b_recv, w_recv) {
        (Ok(b), Ok(w)) => (b, w),
        (got_black, got_white) => {
            tracing::warn!(
                game_name = %game_name.as_str(),
                black = %black_handle,
                white = %white_handle,
                // `Result::err()` で `Option<TransportHandoffError>` を取り出して
                // `Debug` 出力する。`None` は recv 成功側、
                // `Some(SenderDropped)` / `Some(TimedOut)` が失敗理由の内訳。
                black_recv_err = ?got_black.as_ref().err(),
                white_recv_err = ?got_white.as_ref().err(),
                "scheduled match handoff: transport recv failed; aborting"
            );
            if let Ok(mut surviving) = got_black {
                notify_aborted_match(&mut surviving, &game_name).await;
            }
            if let Ok(mut surviving) = got_white {
                notify_aborted_match(&mut surviving, &game_name).await;
            }
            logout_pair(&state, &black_handle, &white_handle).await;
            return;
        }
    };

    // drive_game は black の done_tx を消費して終局通知する。white の done_tx は
    // 本関数のクロージャに残し、drive 完了後に手動 signal する。
    let game_name_for_task = game_name.clone();
    let black_handle_for_task = black_handle.clone();
    let white_handle_for_task = white_handle.clone();
    tokio::task::spawn_local(async move {
        let result = drive_game(
            state,
            // drive_game の引数は (opp_*, self_*) 並び。Floodgate 経路では
            // どちらも waiter なので役割は対称。black を「opp 役」（drive_game が
            // done_tx を消費する側）に割り当てる。
            b_transport,
            black_handle_for_task.clone(),
            Color::Black,
            w_transport,
            white_handle_for_task.clone(),
            Color::White,
            game_name_for_task,
            None, // initial_sfen — buoy/駒落ち は本タスクの範囲外
            b_done_tx,
        )
        .await;
        let _ = w_done_tx.send(());
        if let Err(e) = result {
            tracing::error!(
                error = %e,
                black = %black_handle_for_task,
                white = %white_handle_for_task,
                "scheduled drive_game returned error"
            );
        }
    });
}

/// abort 経路で確定した transport に対し、`##[ERROR]` 通知を 1 行送ってから
/// 接続を閉じる。送信失敗（既に切断済み等）は best-effort で無視する。
async fn notify_aborted_match(transport: &mut TcpTransport, game_name: &GameName) {
    let line = CsaLine::new(format!(
        "##[ERROR] scheduled match aborted: opponent disconnected for {game_name}"
    ));
    let _ = transport.send_line(&line).await;
}

/// `drain_for_game_name` で WaitingPool から取り出した両 player を League から
/// logout する。drive_game に到達できなかった経路で必ず呼ぶ（League 孤児化防止）。
async fn logout_pair<R, K, P>(state: &SharedState<R, K, P>, black: &str, white: &str)
where
    R: RateStorage + 'static,
    K: KifuStorage + 'static,
    P: PasswordStore + 'static,
{
    let mut league = state.league.lock().await;
    league.logout(&PlayerName::new(black));
    league.logout(&PlayerName::new(white));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_strategy` が `"direct"` を受理し、その他はエラーにする契約を固定。
    /// 15.2 で `"least_diff"` 等を追加するときに本テストを更新する。
    #[test]
    fn build_strategy_accepts_direct_and_rejects_unknown() {
        // `dyn PairingLogic` は Debug 非実装なので `unwrap` ベースのアサートは
        // 通らない。match で取り出して name() を確認する。
        match build_strategy("direct") {
            Ok(s) => assert_eq!(s.name(), "direct"),
            Err(e) => panic!("direct must be accepted, got Err: {e}"),
        }

        let err = expect_err(build_strategy("least_diff"));
        assert!(err.contains("least_diff"), "error must mention input: {err}");

        let err = expect_err(build_strategy("unknown"));
        assert!(err.contains("\"unknown\""));
    }

    fn expect_err(r: Result<Box<dyn PairingLogic>, String>) -> String {
        match r {
            Ok(_) => panic!("expected Err but got Ok"),
            Err(e) => e,
        }
    }

    /// `TokioFloodgateTimer::wait_until` が「過去 deadline」を渡されても即座に
    /// return する契約を固定。spurious tick やシステム時刻ジャンプで scheduler
    /// が無限ループ的に長時間スリープするのを防ぐ。
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn tokio_timer_wait_until_returns_immediately_for_past_deadline() {
        let timer = TokioFloodgateTimer;
        let past = Utc::now() - chrono::Duration::seconds(10);
        // start_paused のため `tokio::time::sleep` は明示的に進めないと進まないが、
        // `dur.is_zero()` 早期 return 経路を通るため即 return する。
        timer.wait_until(past).await;
    }

    /// `recv_transport_with_timeout` の sender drop 経路: oneshot の sender 側
    /// が drop されたら `Err(SenderDropped)` で即 return する（waiter loop が
    /// transport を送る前に exit したケースの再現）。
    ///
    /// `TcpTransport` は `Debug` 非実装なので `Result` を `{:?}` で出さず、
    /// `err()` 側だけ取り出して assert する。
    #[tokio::test(flavor = "current_thread")]
    async fn recv_transport_with_timeout_returns_sender_dropped_when_tx_drops() {
        let (tx, rx) = oneshot::channel::<TcpTransport>();
        drop(tx);
        let err = recv_transport_with_timeout(rx)
            .await
            .err()
            .expect("sender drop must produce Err");
        assert!(
            matches!(err, TransportHandoffError::SenderDropped),
            "expected SenderDropped, got: {err:?}"
        );
    }

    /// `recv_transport_with_timeout` の timeout 経路: tokio start_paused で
    /// 仮想時刻を `TRANSPORT_HANDOFF_TIMEOUT` 超過まで進め、`Err(TimedOut)` を
    /// 返すことを固定する。waiter 側が deadlock した場合に scheduler ループが
    /// 永久ブロックしないことを保証する。
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn recv_transport_with_timeout_returns_timed_out_on_no_response() {
        let (_tx, rx) = oneshot::channel::<TcpTransport>();
        // sender (`_tx`) は drop せず保持したまま、仮想時刻だけ進める。
        // `start_paused` 下では `tokio::time::timeout` が auto-advance で
        // 即座に発火する（runtime が「次に進めるべき時刻」を検出する）。
        let err = recv_transport_with_timeout(rx)
            .await
            .err()
            .expect("timeout must produce Err");
        assert!(
            matches!(err, TransportHandoffError::TimedOut),
            "expected TimedOut, got: {err:?}"
        );
    }
}
