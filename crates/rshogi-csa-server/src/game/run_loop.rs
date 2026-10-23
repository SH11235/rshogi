//! `GameRoom` を `tokio` 非同期ランタイムで駆動するループ層。
//!
//! - 2 つの [`crate::port::ClientTransport`] と 1 つの [`crate::port::Broadcaster`] を
//!   受け取り、対局終了まで行受信 → `handle_line` → 配信 → 状態遷移を回す。
//! - 行受信と時間切れアラームを `tokio::select!` で同時待ちする。
//! - 配信先のマッピング（[`crate::game::room::BroadcastTarget`] → 物理経路）を
//!   ここで一元化する（Spectators / All の経路を配信層側で明文化する意図）。
//! - I/O が必要なため、本モジュールは `tokio-transport` フィーチャ下でのみコンパイルされる。

use std::time::Duration;

use tokio::time::{Instant, sleep_until};

use crate::error::{ServerError, TransportError};
use crate::game::result::GameResult;
use crate::game::room::{BroadcastEntry, BroadcastTarget, GameRoom, GameStatus, HandleOutcome};
use crate::port::{BroadcastTag, Broadcaster, ClientTransport};
use crate::types::{Color, RoomId};

/// `recv_line` に渡す「実質無限」のタイムアウト（10 年）。手番外の対局者は
/// この値で待機し、外側の `sleep_until` で手番側の時計切れを別途駆動する。
const NEAR_INFINITE: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 10);

/// 通信遅延を見越した時計超過の追加猶予。手番側の残時間が 0 になっても
/// この時間内なら `recv_line` を待ち続け、ネットワーク往復のばらつきを吸収する。
const TIMEUP_GRACE_MS: u64 = 250;

/// `GameRoom` を `tokio` で回すランループ。
///
/// `now_ms` は `GameRoom` の手番開始時刻と同じ単調時計域のミリ秒時刻を返す。
/// テストでは部屋の開始時刻と同期した仮想時計を渡す。
/// 対局終了まで処理し、最終結果を返す。
///
/// # 配信規約
///
/// - `BroadcastTarget::Black` / `White` → 該当 `ClientTransport::send_line`（個別宛）
/// - `BroadcastTarget::Players` → 両 `ClientTransport::send_line`
/// - `BroadcastTarget::Spectators` → `Broadcaster::broadcast_tag(Spectator)`
/// - `BroadcastTarget::All` → 両 `ClientTransport::send_line` + `broadcast_tag(Spectator)`
///
/// `Broadcaster::broadcast_room` は使わない（対局者へ二重配信されてしまうため）。
pub async fn run_room<TBlack, TWhite, B, F>(
    room: &mut GameRoom,
    sente_io: &mut TBlack,
    gote_io: &mut TWhite,
    broadcaster: &B,
    room_id: &RoomId,
    mut now_ms: F,
) -> Result<GameResult, ServerError>
where
    TBlack: ClientTransport,
    TWhite: ClientTransport,
    B: Broadcaster,
    F: FnMut() -> u64,
{
    loop {
        // 1. 終局チェック。`Finished` 状態なら結果を取り出して終了。
        if let GameStatus::Finished(result) = room.status() {
            return Ok(result.clone());
        }

        // 2. 手番側の時計切れ deadline を計算。`Playing` 中のみ有効で、それ以外は
        //    `NEAR_INFINITE` を待つ（AGREE 待ち中など）。
        let deadline = compute_deadline(room, now_ms());

        // 3. 行受信 or 時間切れアラームを race。
        let event = tokio::select! {
            r = sente_io.recv_line(NEAR_INFINITE) => Event::Recv(Color::Black, r),
            r = gote_io.recv_line(NEAR_INFINITE) => Event::Recv(Color::White, r),
            _ = sleep_until(deadline) => Event::TimeUp,
        };

        let handle_result = match event {
            Event::Recv(from, Ok(line)) => room.handle_line(from, &line, now_ms())?,
            Event::Recv(from, Err(TransportError::Closed)) => room.force_abnormal(from),
            Event::Recv(from, Err(TransportError::Timeout)) => {
                // recv_line に NEAR_INFINITE を渡しているため通常起きない。
                // 起きた場合はアダプタ独自のアイドルタイムアウトと推定し、当該対局者を
                // 切断扱いにして異常終了させる（勝敗側を取り違えないよう `from` を使う）。
                room.force_abnormal(from)
            }
            Event::Recv(_, Err(e)) => return Err(ServerError::Transport(e)),
            Event::TimeUp => {
                let loser_core = room.position().side_to_move();
                let loser: Color = loser_core.into();
                room.force_time_up(loser)
            }
        };

        // 4. broadcasts を物理経路に落とし込む。
        dispatch_broadcasts(&handle_result.broadcasts, sente_io, gote_io, broadcaster, room_id)
            .await?;

        // 5. 終局を検出したら結果を返す。
        if let HandleOutcome::GameEnded(result) = handle_result.outcome {
            return Ok(result);
        }
    }
}

enum Event {
    /// 当該対局者から 1 行（または I/O エラー）が届いた。
    Recv(Color, Result<crate::types::CsaLine, TransportError>),
    /// 手番側の時計切れアラーム。
    TimeUp,
}

// 受信のたびに猶予を付け直さず、手番開始時刻と再接続補償から固定した期限を使う。
fn compute_deadline(room: &GameRoom, now_ms: u64) -> Instant {
    let Some(deadline_ms) = room.current_turn_deadline_ms() else {
        return Instant::now() + NEAR_INFINITE;
    };
    let wait_ms = deadline_ms
        .saturating_add(room.time_margin_ms())
        .saturating_add(TIMEUP_GRACE_MS)
        .saturating_sub(now_ms);
    Instant::now() + Duration::from_millis(wait_ms)
}

async fn dispatch_broadcasts<TBlack, TWhite, B>(
    entries: &[BroadcastEntry],
    sente_io: &mut TBlack,
    gote_io: &mut TWhite,
    broadcaster: &B,
    room_id: &RoomId,
) -> Result<(), ServerError>
where
    TBlack: ClientTransport,
    TWhite: ClientTransport,
    B: Broadcaster,
{
    for entry in entries {
        match entry.target {
            BroadcastTarget::Black => {
                sente_io.send_line(&entry.line).await?;
            }
            BroadcastTarget::White => {
                gote_io.send_line(&entry.line).await?;
            }
            BroadcastTarget::Players => {
                sente_io.send_line(&entry.line).await?;
                gote_io.send_line(&entry.line).await?;
            }
            BroadcastTarget::Spectators => {
                broadcaster.broadcast_tag(room_id, BroadcastTag::Spectator, &entry.line).await?;
            }
            BroadcastTarget::All => {
                sente_io.send_line(&entry.line).await?;
                gote_io.send_line(&entry.line).await?;
                broadcaster.broadcast_tag(room_id, BroadcastTag::Spectator, &entry.line).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::clock::{ClockSpec, SecondsCountdownClock};
    use crate::game::room::GameRoomConfig;
    use crate::types::{CsaLine, GameId, IpKey, PlayerName};
    use rshogi_core::types::EnteringKingRule;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

    /// テスト用の単方向 transport。
    /// - `tx` で外部から行（または I/O エラー）を 1 件ずつ送り込み、`recv_line` で 1 件ずつ受け取る。
    ///   `tokio::sync::mpsc` を使うことで `select!` の競合を制御し、
    ///   テスト側が `sleep` を挟むことで対局者の発話順を確定できる。
    /// - `outbox` には `send_line` で書き込まれた行が記録される。
    /// - 送信ハンドル `tx` がドロップされて受信が `None` を返したら `TransportError::Closed`。
    type Outbox = Rc<RefCell<Vec<CsaLine>>>;

    struct MockTransport {
        rx: UnboundedReceiver<Result<CsaLine, TransportError>>,
        outbox: Outbox,
        peer: IpKey,
    }

    struct MockHandles {
        transport: MockTransport,
        tx: UnboundedSender<Result<CsaLine, TransportError>>,
        outbox: Outbox,
    }

    impl MockHandles {
        fn build(label: &str) -> MockHandles {
            let (tx, rx) = unbounded_channel();
            let outbox = Rc::new(RefCell::new(Vec::new()));
            let transport = MockTransport {
                rx,
                outbox: outbox.clone(),
                peer: IpKey::new(label),
            };
            MockHandles {
                transport,
                tx,
                outbox,
            }
        }
    }

    impl ClientTransport for MockTransport {
        async fn recv_line(&mut self, _timeout: Duration) -> Result<CsaLine, TransportError> {
            match self.rx.recv().await {
                Some(item) => item,
                None => Err(TransportError::Closed),
            }
        }

        async fn send_line(&mut self, line: &CsaLine) -> Result<(), TransportError> {
            self.outbox.borrow_mut().push(line.clone());
            Ok(())
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }

        fn peer_id(&self) -> IpKey {
            self.peer.clone()
        }
    }

    struct MockBroadcaster {
        sent: Rc<RefCell<Vec<(BroadcastTag, CsaLine)>>>,
    }

    impl Broadcaster for MockBroadcaster {
        async fn broadcast_room(
            &self,
            _room_id: &RoomId,
            _line: &CsaLine,
        ) -> Result<(), TransportError> {
            Ok(())
        }

        async fn broadcast_tag(
            &self,
            _room_id: &RoomId,
            tag: BroadcastTag,
            line: &CsaLine,
        ) -> Result<(), TransportError> {
            self.sent.borrow_mut().push((tag, line.clone()));
            Ok(())
        }
    }

    fn make_room() -> GameRoom {
        let config = GameRoomConfig {
            game_id: GameId::new("g1"),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            max_moves: 256,
            time_margin_ms: 0,
            entering_king_rule: EnteringKingRule::Point24,
            initial_sfen: None,
        };
        let clock = Box::new(SecondsCountdownClock::new(60, 5));
        GameRoom::new(config, clock).expect("valid test config")
    }

    fn line(s: &str) -> CsaLine {
        CsaLine::new(s)
    }

    /// 1 ステップ分の擬似時間を進めるための短い sleep（仮想時計上で 10ms）。
    /// `select!` の決定論的な進行を確保するため、各 push の間に挟む。
    async fn advance() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_completes_on_toryo() {
        let mut room = make_room();
        let MockHandles {
            transport: mut sente,
            tx: sente_tx,
            outbox: sente_out,
        } = MockHandles::build("sente");
        let MockHandles {
            transport: mut gote,
            tx: gote_tx,
            outbox: gote_out,
        } = MockHandles::build("gote");
        let bcast_log = Rc::new(RefCell::new(Vec::new()));
        let bcast = MockBroadcaster {
            sent: bcast_log.clone(),
        };

        let driver = async {
            // シナリオ: 双方 AGREE → +7776FU (sente) → -3334FU (gote) → %TORYO (sente)
            // 各 push 後に sleep を入れ、ランループが必ず一手分進むようにする。
            sente_tx.send(Ok(line("AGREE"))).unwrap();
            advance().await;
            gote_tx.send(Ok(line("AGREE"))).unwrap();
            advance().await;
            sente_tx.send(Ok(line("+7776FU"))).unwrap();
            advance().await;
            gote_tx.send(Ok(line("-3334FU"))).unwrap();
            advance().await;
            sente_tx.send(Ok(line("%TORYO"))).unwrap();
        };

        let room_id = RoomId::new("g1");
        let start = Instant::now();
        let runner = run_room(&mut room, &mut sente, &mut gote, &bcast, &room_id, || {
            start.elapsed().as_millis() as u64
        });
        let (_, result) = tokio::join!(driver, runner);
        let result = result.unwrap();

        assert!(matches!(
            result,
            GameResult::Toryo {
                winner: Color::White
            }
        ));
        // sente 側 outbox に少なくとも START と移動配信、終局メッセージが届いている。
        let sout = sente_out.borrow();
        assert!(sout.iter().any(|l| l.as_str() == "START:g1"));
        assert!(sout.iter().any(|l| l.as_str() == "+7776FU,T0"));
        assert!(sout.iter().any(|l| l.as_str() == "#RESIGN"));
        // sente は loser → "#LOSE"
        assert!(sout.iter().any(|l| l.as_str() == "#LOSE"));
        // gote 側にも対称な配信が届く。
        let gout = gote_out.borrow();
        assert!(gout.iter().any(|l| l.as_str() == "#RESIGN"));
        assert!(gout.iter().any(|l| l.as_str() == "#WIN"));
        // 観戦者宛 (Spectator tag) も発火しているが本ケースでは受信者ゼロ。
        let spec = bcast_log.borrow();
        assert!(
            spec.iter()
                .any(|(t, l)| *t == BroadcastTag::Spectator && l.as_str() == "#RESIGN")
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_returns_time_up_when_side_clock_expires() {
        let mut room = make_room();
        let MockHandles {
            transport: mut sente,
            tx: sente_tx,
            outbox: _sente_out,
        } = MockHandles::build("sente");
        let MockHandles {
            transport: mut gote,
            tx: gote_tx,
            outbox: _gote_out,
        } = MockHandles::build("gote");
        let bcast = MockBroadcaster {
            sent: Rc::new(RefCell::new(Vec::new())),
        };

        let driver = async {
            // 双方 AGREE で対局開始 → sente は以降何も送らない → 65 秒で時間切れ。
            sente_tx.send(Ok(line("AGREE"))).unwrap();
            advance().await;
            gote_tx.send(Ok(line("AGREE"))).unwrap();
        };

        let room_id = RoomId::new("g1");
        let start = Instant::now();
        let runner = run_room(&mut room, &mut sente, &mut gote, &bcast, &room_id, || {
            start.elapsed().as_millis() as u64
        });
        let (_, result) = tokio::join!(driver, runner);
        let result = result.unwrap();

        // 60 秒 + 5 秒の秒読みを使い切って sente の時間切れ → loser=Black。
        assert!(matches!(
            result,
            GameResult::TimeUp {
                loser: Color::Black
            }
        ));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_allows_byoyomi_after_main_time_exhausted() {
        let config = GameRoomConfig {
            game_id: GameId::new("g1"),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            max_moves: 256,
            time_margin_ms: 0,
            entering_king_rule: EnteringKingRule::Point24,
            initial_sfen: None,
        };
        let clock = Box::new(SecondsCountdownClock::new(2, 10));
        let mut room = GameRoom::new(config, clock).expect("valid test config");
        let MockHandles {
            transport: mut sente,
            tx: sente_tx,
            outbox: _sente_out,
        } = MockHandles::build("sente");
        let MockHandles {
            transport: mut gote,
            tx: gote_tx,
            outbox: _gote_out,
        } = MockHandles::build("gote");
        let bcast = MockBroadcaster {
            sent: Rc::new(RefCell::new(Vec::new())),
        };

        let driver = async {
            sente_tx.send(Ok(line("AGREE"))).unwrap();
            advance().await;
            gote_tx.send(Ok(line("AGREE"))).unwrap();
            tokio::time::sleep(Duration::from_secs(8)).await;
            sente_tx.send(Ok(line("+7776FU"))).unwrap();
            advance().await;
            gote_tx.send(Ok(line("%TORYO"))).unwrap();
        };

        let room_id = RoomId::new("g1");
        let start = tokio::time::Instant::now();
        let runner = run_room(&mut room, &mut sente, &mut gote, &bcast, &room_id, || {
            tokio::time::Instant::now().saturating_duration_since(start).as_millis() as u64
        });
        let (_, result) = tokio::join!(driver, runner);
        let result = result.unwrap();
        // 秒読み内で sente の手が通り、gote 投了で sente 勝利。
        assert!(matches!(
            result,
            GameResult::Toryo {
                winner: Color::Black,
            }
        ));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_returns_abnormal_when_player_disconnects() {
        let mut room = make_room();
        let MockHandles {
            transport: mut sente,
            tx: sente_tx,
            outbox: _sente_out,
        } = MockHandles::build("sente");
        let MockHandles {
            transport: mut gote,
            tx: gote_tx,
            outbox: _gote_out,
        } = MockHandles::build("gote");
        let bcast = MockBroadcaster {
            sent: Rc::new(RefCell::new(Vec::new())),
        };

        let driver = async {
            // 双方 AGREE で Playing 入り → sente が切断（Closed）を送る。
            sente_tx.send(Ok(line("AGREE"))).unwrap();
            advance().await;
            gote_tx.send(Ok(line("AGREE"))).unwrap();
            advance().await;
            sente_tx.send(Err(TransportError::Closed)).unwrap();
        };

        let room_id = RoomId::new("g1");
        let start = Instant::now();
        let runner = run_room(&mut room, &mut sente, &mut gote, &bcast, &room_id, || {
            start.elapsed().as_millis() as u64
        });
        let (_, result) = tokio::join!(driver, runner);
        let result = result.unwrap();

        // Playing 中の sente 切断 → winner=White の Abnormal。
        assert!(matches!(
            result,
            GameResult::Abnormal {
                winner: Some(Color::White),
            }
        ));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_charges_real_elapsed_even_within_communication_margin() {
        let config = GameRoomConfig {
            game_id: GameId::new("g"),
            black: PlayerName::new("a"),
            white: PlayerName::new("b"),
            max_moves: 256,
            // 持ち時間 1 秒 + 秒読み 0 秒 + 通信マージン 5 秒。
            time_margin_ms: 5_000,
            entering_king_rule: EnteringKingRule::Point24,
            initial_sfen: None,
        };
        let clock = Box::new(SecondsCountdownClock::new(1, 0));
        let mut room = GameRoom::new(config, clock).expect("valid test config");
        let MockHandles {
            transport: mut sente,
            tx: sente_tx,
            outbox: _sente_out,
        } = MockHandles::build("sente");
        let MockHandles {
            transport: mut gote,
            tx: gote_tx,
            outbox: _gote_out,
        } = MockHandles::build("gote");
        let bcast = MockBroadcaster {
            sent: Rc::new(RefCell::new(Vec::new())),
        };

        let driver = async {
            sente_tx.send(Ok(line("AGREE"))).unwrap();
            advance().await;
            gote_tx.send(Ok(line("AGREE"))).unwrap();
            // Playing 開始を 0 として 4 秒待機後に着手。deadline は 7.249 秒なので手は
            // handle_line まで届くが、実時間 4 秒課金で 1 秒予算を超過して time-up する。
            tokio::time::sleep(Duration::from_secs(4)).await;
            sente_tx.send(Ok(line("+7776FU"))).unwrap();
            // sente が既に time-up 済みなので、この %TORYO は届く前に対局が終わる。
            advance().await;
            gote_tx.send(Ok(line("%TORYO"))).unwrap();
        };

        let room_id = RoomId::new("g1");
        // now_ms はループ毎に呼ばれる。仮想時計に同期するため tokio::time::Instant 経由で計算。
        let start = tokio::time::Instant::now();
        let runner = run_room(&mut room, &mut sente, &mut gote, &bcast, &room_id, || {
            tokio::time::Instant::now().saturating_duration_since(start).as_millis() as u64
        });
        let (_, result) = tokio::join!(driver, runner);
        let result = result.unwrap();

        // sente の +7776FU は deadline 内で届くが、実時間 4 秒課金で 1 秒予算を超過 → time-up。
        assert!(matches!(
            result,
            GameResult::TimeUp {
                loser: Color::Black
            }
        ));
    }

    fn ready_room(clock: ClockSpec, margin_ms: u64, started_ms: u64) -> GameRoom {
        let config = GameRoomConfig {
            game_id: GameId::new("g1"),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            max_moves: 256,
            time_margin_ms: margin_ms,
            entering_king_rule: EnteringKingRule::Point24,
            initial_sfen: None,
        };
        let mut room = GameRoom::new(config, clock.build_clock()).unwrap();
        room.handle_line(Color::Black, &line("AGREE"), started_ms).unwrap();
        room.handle_line(Color::White, &line("AGREE"), started_ms).unwrap();
        room
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn keepalives_preserve_deadline_including_margin_and_reconnect_credit() {
        let mut room = ready_room(
            ClockSpec::Countdown {
                total_time_sec: 1,
                byoyomi_sec: 0,
            },
            500,
            10_000,
        );
        room.restore_current_turn_reconnect_credit_ms(700, 700);
        let mut sente = MockHandles::build("sente");
        let mut gote = MockHandles::build("gote");
        let bcast = MockBroadcaster {
            sent: Rc::new(RefCell::new(Vec::new())),
        };
        let room_id = RoomId::new("g1");
        let start = Instant::now();
        let driver = async {
            for at_ms in [500, 1_000, 2_000, 3_000, 3_400] {
                sleep_until(start + Duration::from_millis(at_ms)).await;
                sente.tx.send(Ok(line(""))).unwrap();
                gote.tx.send(Ok(line(""))).unwrap();
            }
        };
        let runner = async {
            let result = run_room(
                &mut room,
                &mut sente.transport,
                &mut gote.transport,
                &bcast,
                &room_id,
                || 10_000 + start.elapsed().as_millis() as u64,
            )
            .await;
            (result, start.elapsed())
        };
        let (_, (result, elapsed)) = tokio::join!(driver, runner);
        assert!(matches!(
            result.unwrap(),
            GameResult::TimeUp {
                loser: Color::Black
            }
        ));
        // 秒切り捨て猶予999ms、通信マージン500ms、再接続補償700ms、通信猶予250ms。
        assert!(
            (Duration::from_millis(3_449)..=Duration::from_millis(3_450)).contains(&elapsed),
            "{elapsed:?}"
        );
        assert!(sente.outbox.borrow().iter().any(|line| line.as_str() == "#TIME_UP"));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn turn_changes_reset_byoyomi_and_preserve_fischer_increment() {
        for (clock, expected_ms) in [
            (
                ClockSpec::Countdown {
                    total_time_sec: 2,
                    byoyomi_sec: 3,
                },
                8_249,
            ),
            (
                ClockSpec::Fischer {
                    total_time_sec: 3,
                    increment_sec: 3,
                },
                12_249,
            ),
        ] {
            let mut room = ready_room(clock, 0, 0);
            let mut sente = MockHandles::build("sente");
            let mut gote = MockHandles::build("gote");
            let bcast = MockBroadcaster {
                sent: Rc::new(RefCell::new(Vec::new())),
            };
            let room_id = RoomId::new("g1");
            let start = Instant::now();
            let driver = async {
                sleep_until(start + Duration::from_millis(1_500)).await;
                sente.tx.send(Ok(line("+7776FU"))).unwrap();
                sleep_until(start + Duration::from_millis(3_000)).await;
                gote.tx.send(Ok(line("-3334FU"))).unwrap();
                sleep_until(start + Duration::from_millis(4_000)).await;
                sente.tx.send(Ok(line(""))).unwrap();
            };
            let runner = async {
                let result = run_room(
                    &mut room,
                    &mut sente.transport,
                    &mut gote.transport,
                    &bcast,
                    &room_id,
                    || start.elapsed().as_millis() as u64,
                )
                .await;
                (result, start.elapsed())
            };
            let (_, (result, elapsed)) = tokio::join!(driver, runner);
            assert!(matches!(
                result.unwrap(),
                GameResult::TimeUp {
                    loser: Color::Black
                }
            ));
            assert!(
                (Duration::from_millis(expected_ms)..=Duration::from_millis(expected_ms + 2))
                    .contains(&elapsed),
                "{elapsed:?}"
            );
            let outbox = sente.outbox.borrow();
            assert!(outbox.iter().any(|line| line.as_str() == "+7776FU,T1"));
            assert!(outbox.iter().any(|line| line.as_str() == "-3334FU,T1"));
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn expired_deadline_does_not_grant_margin_or_grace_again() {
        let mut room = ready_room(
            ClockSpec::Countdown {
                total_time_sec: 1,
                byoyomi_sec: 0,
            },
            500,
            10_000,
        );
        let mut sente = MockHandles::build("sente");
        let mut gote = MockHandles::build("gote");
        let bcast = MockBroadcaster {
            sent: Rc::new(RefCell::new(Vec::new())),
        };
        let room_id = RoomId::new("g1");
        let start = Instant::now();
        let result = run_room(
            &mut room,
            &mut sente.transport,
            &mut gote.transport,
            &bcast,
            &room_id,
            || 13_000 + start.elapsed().as_millis() as u64,
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            GameResult::TimeUp {
                loser: Color::Black
            }
        ));
        assert!(start.elapsed() <= Duration::from_millis(1));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn invalid_input_returns_error_without_restarting_turn_clock() {
        let mut room = ready_room(
            ClockSpec::Countdown {
                total_time_sec: 1,
                byoyomi_sec: 0,
            },
            0,
            0,
        );
        let deadline = room.current_turn_deadline_ms();
        let mut sente = MockHandles::build("sente");
        let mut gote = MockHandles::build("gote");
        let bcast = MockBroadcaster {
            sent: Rc::new(RefCell::new(Vec::new())),
        };
        let room_id = RoomId::new("g1");
        let start = Instant::now();
        for at_ms in [500, 1_000, 1_500] {
            let driver = async {
                sleep_until(start + Duration::from_millis(at_ms)).await;
                sente.tx.send(Ok(line("AGREE"))).unwrap();
            };
            let runner = run_room(
                &mut room,
                &mut sente.transport,
                &mut gote.transport,
                &bcast,
                &room_id,
                || start.elapsed().as_millis() as u64,
            );
            let (_, result) = tokio::join!(driver, runner);
            assert!(matches!(result, Err(ServerError::State(_))));
            assert_eq!(room.current_turn_deadline_ms(), deadline);
            assert!(start.elapsed() <= Duration::from_millis(at_ms + 1));
        }
        let result = run_room(
            &mut room,
            &mut sente.transport,
            &mut gote.transport,
            &bcast,
            &room_id,
            || start.elapsed().as_millis() as u64,
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            GameResult::TimeUp {
                loser: Color::Black
            }
        ));
        assert!(
            (Duration::from_millis(2_249)..=Duration::from_millis(2_250))
                .contains(&start.elapsed())
        );
    }
}
