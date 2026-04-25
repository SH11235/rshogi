//! Durable Object 永続化レイヤと cold start 復元の純粋ロジック。
//!
//! Cloudflare DO は isolate 単位で破棄されるため、対局状態は外部ストレージに
//! 永続化しておき、再構築時に [`replay_core_room`] で `CoreRoom` を復元する。
//! 本モジュールは I/O を持たず、永続化済みデータ構造から
//! `rshogi_csa_server::GameRoom`（以下 `CoreRoom`）を組み直す手順だけを担う。
//!
//! 永続化レイヤの分担:
//!
//! - `slots` (= [`crate::session_state::Slot`]): WebSocket 別の役割割当
//! - [`PersistedConfig`]: マッチ成立時に確定する対局メタ + 開始局面 SFEN
//! - `moves` テーブル ([`MoveRow`]): ply 順の指し手列（SQL）
//! - [`FinishedState`]: 終局確定後のフラグ（同 DO で再起動した場合の早期 return）
//!
//! cold start 復元の手順は [`replay_core_room`] のドキュメントを参照。

use serde::{Deserialize, Serialize};

use rshogi_core::types::EnteringKingRule;
use rshogi_csa_server::ClockSpec;
use rshogi_csa_server::game::room::{GameRoom as CoreRoom, GameRoomConfig};
use rshogi_csa_server::types::{Color, CsaLine, GameId, PlayerName};

/// マッチ成立時に永続化する対局設定。`CoreRoom` の再構築に必要な最小情報。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedConfig {
    /// 対局 ID。`<room_id>-<epoch_ms>` 形式で `start_match` が生成する。
    pub(crate) game_id: String,
    /// 先手プレイヤのハンドル。
    pub(crate) black_handle: String,
    /// 後手プレイヤのハンドル。
    pub(crate) white_handle: String,
    /// LOGIN ハンドル末尾の `<game_name>`。マッチ確認・棋譜メタに使う。
    pub(crate) game_name: String,
    /// 時計設定（Countdown / Fischer / StopWatch）。
    pub(crate) clock: ClockSpec,
    /// 最大手数。
    pub(crate) max_moves: u32,
    /// 通信マージン（ミリ秒）。
    pub(crate) time_margin_ms: u64,
    /// マッチ成立（2 人目の LOGIN 受理）時刻。`$START_TIME` 等の参考に使う。
    pub(crate) matched_at_ms: u64,
    /// 両者 AGREE を受理して `HandleOutcome::GameStarted` が立った瞬間。
    /// `None` の間は `AgreeWaiting`/`StartWaiting` 段階で、cold start 復元時も
    /// `CoreRoom` は `AgreeWaiting` で作り直す。`Some(t)` になって初めて replay
    /// で AGREE を再送して `Playing` 状態に戻す（START 後・初手前の再起動対策）。
    pub(crate) play_started_at_ms: Option<u64>,
    /// 対局の開始局面 SFEN。通常対局は `None` (= 平手)。buoy / `%%FORK` 経由の
    /// 対局では `Some(sfen)` で、cold start 復元時もこの SFEN から `CoreRoom` を
    /// 組み直す。
    pub(crate) initial_sfen: Option<String>,
}

/// 終局フラグ。一度 `Some` になったらその DO は同じ対局を二度開始しない。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishedState {
    /// CSA 終局コード（`#RESIGN` / `#TIME_UP` / `#ILLEGAL_MOVE` 等）。
    pub(crate) result_code: String,
    /// 終局確定時刻（UNIX エポック ミリ秒）。
    pub(crate) ended_at_ms: u64,
}

/// `moves` SQL テーブル 1 行分。replay / alarm で使う。
///
/// `Serialize` も付けてあるのは、ホスト target のテストで replay 入力を直接
/// 構築するため。実 DO 上では `cursor.to_array::<MoveRow>()` で読み込むだけで
/// `Serialize` は使わない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveRow {
    /// 1 始まりの手数。`COALESCE(MAX(ply), 0) + 1` で採番される。
    pub(crate) ply: i64,
    /// 手番色。`"black"` または `"white"` のみ受理する。
    pub(crate) color: String,
    /// CSA 1 行（例: `"+7776FU,T3"`）。`CsaLine` のラップ前 raw 文字列。
    pub(crate) line: String,
    /// 手を受信した瞬間の wall-clock ミリ秒。replay の clock 復元に使う。
    pub(crate) at_ms: i64,
}

/// `replay_core_room` の戻り値。cold start 復元の各分岐をデータとして表現する。
///
/// DO 側 (`game_room.rs::ensure_core_loaded`) はこれをパターンマッチして
/// `Restored` のみコアを採用、それ以外はログ出力のうえコア未生成のまま返す。
/// 失敗系は console_log 用の文字列を保持しておき、運用時に `wrangler tail`
/// で原因を特定できるようにする。
#[derive(Debug)]
pub enum ReplaySummary {
    /// 復元に成功した。
    ///
    /// `CoreRoom` は内部に `Position` 等を抱えてサイズが大きい (~1.3KB) ため、
    /// 失敗系の小さい variant とのサイズ差を抑える目的で `Box` でくるんで持つ
    /// (clippy::large_enum_variant)。
    Restored {
        /// 復元済み `CoreRoom`。`AgreeWaiting`（`play_started_at_ms = None`）または
        /// `Playing`（AGREE 再送後）のどちらかの状態にある。
        core: Box<CoreRoom>,
    },
    /// 開始局面 SFEN が `CoreRoom::new` で拒否された。`reason` は console_log 用。
    InvalidSfen {
        /// `CoreRoom::new` が返したエラー文字列。
        reason: String,
    },
    /// `MoveRow::color` が `"black"` / `"white"` 以外の不明値だった。
    UnknownColor {
        /// 該当 row の `ply`。
        ply: i64,
        /// 受け取った文字列。
        color: String,
    },
    /// 手の replay が `handle_line` で拒否された。盤面整合性の壊れたデータが
    /// 永続化された場合に発火する。
    MoveReplayFailed {
        /// 該当 row の `ply`。
        ply: i64,
        /// 該当 row の生 CSA 行。
        line: String,
        /// `handle_line` のエラー文字列。
        reason: String,
    },
}

/// 永続化済みデータから `CoreRoom` を再構築する純粋関数。
///
/// 流れは以下:
///
/// 1. `cfg.clock.build_clock()` で `TimeClock` を生成
/// 2. `cfg.initial_sfen` を尊重して `CoreRoom::new` で空ルームを作成。SFEN
///    検証に失敗したら [`ReplaySummary::InvalidSfen`] を返す
/// 3. `cfg.play_started_at_ms` が `Some(t)` のとき:
///    - 両側に AGREE を `t` のタイムスタンプで再送して `Playing` に遷移
///    - `moves` を ply 順に逐次 `handle_line` で再生する。各手の wall-clock
///      タイムスタンプは `at_ms.max(0).max(t)` に正規化（負値や AGREE より
///      前の `at_ms` は clock を巻き戻すため）
/// 4. `play_started_at_ms = None` の場合は `AgreeWaiting` のまま返す
///
/// 設計上、本関数は I/O を持たないため、ホスト target で網羅的にテスト可能。
/// 実 DO 経路は本関数の戻り値をパターンマッチするだけのアダプタになる。
pub fn replay_core_room(cfg: &PersistedConfig, moves: &[MoveRow]) -> ReplaySummary {
    let clock = cfg.clock.build_clock();
    let mut core = match CoreRoom::new(
        GameRoomConfig {
            game_id: GameId::new(cfg.game_id.clone()),
            black: PlayerName::new(cfg.black_handle.clone()),
            white: PlayerName::new(cfg.white_handle.clone()),
            max_moves: cfg.max_moves,
            time_margin_ms: cfg.time_margin_ms,
            entering_king_rule: EnteringKingRule::Point24,
            initial_sfen: cfg.initial_sfen.clone(),
        },
        clock,
    ) {
        Ok(c) => c,
        Err(e) => {
            return ReplaySummary::InvalidSfen {
                reason: format!("{e:?}"),
            };
        }
    };

    let Some(play_started_at_ms) = cfg.play_started_at_ms else {
        // AGREE 前のスナップショットからの cold start。CoreRoom は AgreeWaiting で返す。
        return ReplaySummary::Restored {
            core: Box::new(core),
        };
    };

    // `CoreRoom::new` 直後は必ず `AgreeWaiting` 状態で、そこから game_id 省略の
    // AGREE を流すと `verify_game_id(None)` を素通りして `Continue` を返す契約。
    // 失敗するのは Core 側の状態機械が崩れている契約違反ケースのみで、この場合
    // 永続化レイヤから先に進めても整合性が取れないため `expect` で落として
    // bug を顕在化させる。
    for color in [Color::Black, Color::White] {
        core.handle_line(color, &CsaLine::new("AGREE"), play_started_at_ms)
            .expect("CoreRoom::new returns AgreeWaiting; AGREE from there must succeed");
    }

    for m in moves {
        let color = match m.color.as_str() {
            "black" => Color::Black,
            "white" => Color::White,
            other => {
                return ReplaySummary::UnknownColor {
                    ply: m.ply,
                    color: other.to_owned(),
                };
            }
        };
        let ts = (m.at_ms.max(0) as u64).max(play_started_at_ms);
        if let Err(e) = core.handle_line(color, &CsaLine::new(&m.line), ts) {
            return ReplaySummary::MoveReplayFailed {
                ply: m.ply,
                line: m.line.clone(),
                reason: format!("{e:?}"),
            };
        }
    }

    ReplaySummary::Restored {
        core: Box::new(core),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_csa_server::game::result::GameResult;
    use rshogi_csa_server::game::room::{GameStatus, HandleOutcome};
    use rshogi_csa_server::record::kifu::primary_result_code;

    /// `play_started_at_ms` の代表値（適当な epoch ms）。テスト全体で共有。
    const PLAY_STARTED_AT_MS: u64 = 1_000_000;

    fn baseline_config() -> PersistedConfig {
        PersistedConfig {
            game_id: "room-1-test".to_owned(),
            black_handle: "alice".to_owned(),
            white_handle: "bob".to_owned(),
            game_name: "g1".to_owned(),
            clock: ClockSpec::Countdown {
                total_time_sec: 60,
                byoyomi_sec: 10,
            },
            max_moves: 256,
            time_margin_ms: 0,
            matched_at_ms: PLAY_STARTED_AT_MS - 100,
            play_started_at_ms: None,
            initial_sfen: None,
        }
    }

    fn move_row(ply: i64, color: &str, line: &str, at_ms_offset_from_start: u64) -> MoveRow {
        MoveRow {
            ply,
            color: color.to_owned(),
            line: line.to_owned(),
            at_ms: (PLAY_STARTED_AT_MS + at_ms_offset_from_start) as i64,
        }
    }

    /// ホスト側で「同じ AGREE + 同じ手列を直接 `CoreRoom` に流した」結果を作る
    /// helper。replay の出力と直接構築した CoreRoom が状態完全一致することを
    /// テストする際の比較対象として使う。
    fn directly_played(cfg: &PersistedConfig, moves: &[MoveRow]) -> CoreRoom {
        let mut core = CoreRoom::new(
            GameRoomConfig {
                game_id: GameId::new(cfg.game_id.clone()),
                black: PlayerName::new(cfg.black_handle.clone()),
                white: PlayerName::new(cfg.white_handle.clone()),
                max_moves: cfg.max_moves,
                time_margin_ms: cfg.time_margin_ms,
                entering_king_rule: EnteringKingRule::Point24,
                initial_sfen: cfg.initial_sfen.clone(),
            },
            cfg.clock.build_clock(),
        )
        .expect("baseline config must build");
        let Some(t0) = cfg.play_started_at_ms else {
            return core;
        };
        for color in [Color::Black, Color::White] {
            core.handle_line(color, &CsaLine::new("AGREE"), t0)
                .expect("AGREE in directly_played");
        }
        for m in moves {
            let color = match m.color.as_str() {
                "black" => Color::Black,
                "white" => Color::White,
                _ => unreachable!("test data must use black/white only"),
            };
            let ts = (m.at_ms.max(0) as u64).max(t0);
            core.handle_line(color, &CsaLine::new(&m.line), ts)
                .expect("move in directly_played");
        }
        core
    }

    #[test]
    fn replay_without_play_started_returns_agree_waiting_room() {
        let cfg = baseline_config();
        let summary = replay_core_room(&cfg, &[]);
        let ReplaySummary::Restored { core } = summary else {
            panic!("expected Restored, got {summary:?}");
        };
        assert!(matches!(core.status(), GameStatus::AgreeWaiting));
        assert_eq!(core.moves_played(), 0);
    }

    #[test]
    fn replay_with_play_started_and_no_moves_returns_playing_room() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        let summary = replay_core_room(&cfg, &[]);
        let ReplaySummary::Restored { core } = summary else {
            panic!("expected Restored, got {summary:?}");
        };
        assert!(matches!(core.status(), GameStatus::Playing));
        assert_eq!(core.current_turn(), Color::Black);
        assert_eq!(core.moves_played(), 0);
    }

    #[test]
    fn replay_with_three_moves_matches_directly_constructed_core_room() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        let moves = vec![
            move_row(1, "black", "+7776FU,T3", 3_000),
            move_row(2, "white", "-3334FU,T2", 6_000),
            move_row(3, "black", "+8833UM,T4", 11_000),
        ];

        let ReplaySummary::Restored {
            core: replayed_core,
        } = replay_core_room(&cfg, &moves)
        else {
            panic!("expected Restored");
        };

        let direct_core = directly_played(&cfg, &moves);

        assert_eq!(replayed_core.moves_played(), direct_core.moves_played());
        assert_eq!(format!("{:?}", replayed_core.status()), format!("{:?}", direct_core.status()));
        assert_eq!(replayed_core.current_turn(), direct_core.current_turn());
        // Position は SFEN 一致で局面完全一致を担保（盤面 + 持駒 + 手番 + 手数）。
        assert_eq!(replayed_core.position().to_sfen(), direct_core.position().to_sfen());
        // 残時間（本体）が両側で一致する。
        for color in [Color::Black, Color::White] {
            assert_eq!(
                replayed_core.clock_remaining_main_ms(color),
                direct_core.clock_remaining_main_ms(color),
                "remaining_main_ms mismatch for {color:?}"
            );
        }
        // `clock_turn_budget_ms` も比較して deadline 計算用予算（本体 + byoyomi/increment）
        // を含めた時計状態が完全に一致することを確認する。
        for color in [Color::Black, Color::White] {
            assert_eq!(
                replayed_core.clock_turn_budget_ms(color),
                direct_core.clock_turn_budget_ms(color),
                "turn_budget_ms mismatch for {color:?}"
            );
        }
    }

    /// Fischer / StopWatch でも replay 後の `clock_turn_budget_ms` と本体残時間が
    /// 中断なし構築と完全一致する。`clock_remaining_main_ms` だけでは increment や
    /// byoyomi の状態崩壊を捕捉できないため、各 ClockSpec で turn_budget も比較する。
    #[test]
    fn replay_matches_directly_constructed_for_all_clock_specs() {
        let cases = [
            ClockSpec::Countdown {
                total_time_sec: 60,
                byoyomi_sec: 10,
            },
            ClockSpec::Fischer {
                total_time_sec: 60,
                increment_sec: 5,
            },
            ClockSpec::StopWatch {
                total_time_min: 1,
                byoyomi_min: 1,
            },
        ];
        let moves = vec![
            move_row(1, "black", "+7776FU,T3", 3_000),
            move_row(2, "white", "-3334FU,T2", 6_000),
        ];
        for clock in cases {
            let mut cfg = baseline_config();
            cfg.clock = clock.clone();
            cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
            let ReplaySummary::Restored {
                core: replayed_core,
            } = replay_core_room(&cfg, &moves)
            else {
                panic!("expected Restored for {clock:?}");
            };
            let direct_core = directly_played(&cfg, &moves);
            for color in [Color::Black, Color::White] {
                assert_eq!(
                    replayed_core.clock_remaining_main_ms(color),
                    direct_core.clock_remaining_main_ms(color),
                    "remaining_main_ms mismatch for {color:?} under {clock:?}"
                );
                assert_eq!(
                    replayed_core.clock_turn_budget_ms(color),
                    direct_core.clock_turn_budget_ms(color),
                    "turn_budget_ms mismatch for {color:?} under {clock:?}"
                );
            }
        }
    }

    /// 終局直後の cold start 復元シナリオを契約として固定する。`%TORYO` を含む
    /// 手列で復元した CoreRoom は `Finished(GameResult::Toryo)` 状態に到達し、
    /// 中断なし構築側と `primary_result_code` まで一致する。
    #[test]
    fn replay_with_toryo_termination_yields_same_finished_state_as_uninterrupted_play() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        let moves = vec![
            move_row(1, "black", "+7776FU,T3", 3_000),
            move_row(2, "white", "-3334FU,T2", 6_000),
            move_row(3, "black", "%TORYO", 9_000),
        ];

        let ReplaySummary::Restored {
            core: replayed_core,
        } = replay_core_room(&cfg, &moves)
        else {
            panic!("expected Restored");
        };
        let direct_core = directly_played(&cfg, &moves);

        // 状態機械として両方が Finished(Toryo) に到達し、勝敗側も一致する。
        match (replayed_core.status(), direct_core.status()) {
            (
                GameStatus::Finished(GameResult::Toryo {
                    winner: replayed_winner,
                }),
                GameStatus::Finished(GameResult::Toryo {
                    winner: direct_winner,
                }),
            ) => {
                assert_eq!(replayed_winner, direct_winner);
            }
            (a, b) => panic!("expected both Finished(Toryo), got {a:?} / {b:?}"),
        }

        // 棋譜・00LIST に書き出される結果コードも両者で一致する。これが崩れると
        // R2 / FileKifuStorage 経由の `result_code` が cold start 後に書き換わる
        // バグを意味するため、契約としてここで固定する。
        let GameStatus::Finished(replayed_result) = replayed_core.status() else {
            unreachable!()
        };
        let GameStatus::Finished(direct_result) = direct_core.status() else {
            unreachable!()
        };
        assert_eq!(primary_result_code(replayed_result), "#RESIGN");
        assert_eq!(primary_result_code(replayed_result), primary_result_code(direct_result));
    }

    #[test]
    fn replay_then_extra_move_yields_same_outcome_as_uninterrupted_play() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        let played_moves = vec![
            move_row(1, "black", "+7776FU,T3", 3_000),
            move_row(2, "white", "-3334FU,T2", 6_000),
        ];
        // 復元後に 1 手追加（白 → 黒の番なので黒側の手）
        let extra_line = "+2868HI,T5";
        let extra_at_ms = PLAY_STARTED_AT_MS + 9_000;

        let ReplaySummary::Restored {
            core: mut replayed_core,
        } = replay_core_room(&cfg, &played_moves)
        else {
            panic!("expected Restored");
        };
        replayed_core
            .handle_line(Color::Black, &CsaLine::new(extra_line), extra_at_ms)
            .expect("post-replay move must succeed");

        // 中断なしで全手を流した CoreRoom と、replay 後に 1 手追加した CoreRoom を比較。
        let mut continuous_moves = played_moves.clone();
        continuous_moves.push(move_row(3, "black", extra_line, extra_at_ms - PLAY_STARTED_AT_MS));
        let continuous_core = directly_played(&cfg, &continuous_moves);

        assert_eq!(replayed_core.position().to_sfen(), continuous_core.position().to_sfen());
        assert_eq!(replayed_core.moves_played(), continuous_core.moves_played());
        for color in [Color::Black, Color::White] {
            assert_eq!(
                replayed_core.clock_remaining_main_ms(color),
                continuous_core.clock_remaining_main_ms(color),
                "remaining_main_ms mismatch for {color:?} after restart-then-continue"
            );
            assert_eq!(
                replayed_core.clock_turn_budget_ms(color),
                continuous_core.clock_turn_budget_ms(color),
                "turn_budget_ms mismatch for {color:?} after restart-then-continue"
            );
        }
    }

    /// Cold start 後にもう 1 手指して `MoveAccepted` の `remaining_main_ms` まで
    /// 等価になることを確認する（合意済み・初手前 cold start で時計起点が
    /// 巻き戻らないことの直接検証）。
    #[test]
    fn replay_then_first_move_emits_consistent_remaining_main_ms() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        let ReplaySummary::Restored {
            core: mut replayed_core,
        } = replay_core_room(&cfg, &[])
        else {
            panic!("expected Restored");
        };
        let first_move_at_ms = PLAY_STARTED_AT_MS + 4_000;
        let result = replayed_core
            .handle_line(Color::Black, &CsaLine::new("+7776FU,T4"), first_move_at_ms)
            .expect("first move after restart must succeed");
        match result.outcome {
            HandleOutcome::MoveAccepted {
                next_turn,
                remaining_main_ms,
            } => {
                assert_eq!(next_turn, Color::White);
                // Countdown 60s + byoyomi 10s。byoyomi 内で消費した 4 秒は本体から
                // 引かれない契約なので、本体は依然 60_000ms のまま残る。これが
                // 60_000 を割っていれば「play_started_at_ms 起点で時計が巻き戻っ
                // ている」サインとして即座に落とせる。
                assert_eq!(remaining_main_ms, 60_000);
            }
            other => panic!("expected MoveAccepted, got {other:?}"),
        }
    }

    /// `MoveRow::at_ms` が 0 未満や `play_started_at_ms` より小さい異常値でも、
    /// `replay_core_room` が `at_ms.max(0).max(play_started_at_ms)` で正規化する
    /// 結果として時計が巻き戻らないことを直接検証する。`directly_played` ヘルパに
    /// 同じ正規化を入れているので「ヘルパ通しで一致」の系では捕捉できないため、
    /// 固定期待値 60_000ms に対する assert で押さえる。
    #[test]
    fn replay_normalizes_negative_or_pre_start_at_ms() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        // ply=1 は負値、ply=2 は play_started_at_ms より前の絶対時刻、いずれも
        // 正規化後は `play_started_at_ms` ちょうどに丸められる想定。
        let moves = vec![
            MoveRow {
                ply: 1,
                color: "black".to_owned(),
                line: "+7776FU,T0".to_owned(),
                at_ms: -42,
            },
            MoveRow {
                ply: 2,
                color: "white".to_owned(),
                line: "-3334FU,T0".to_owned(),
                at_ms: (PLAY_STARTED_AT_MS - 10_000) as i64,
            },
        ];
        let ReplaySummary::Restored {
            core: replayed_core,
        } = replay_core_room(&cfg, &moves)
        else {
            panic!("expected Restored");
        };
        // 両者とも `play_started_at_ms` ちょうどで指したと扱われ、本体時間は
        // 開始時の 60_000ms から減らない（byoyomi 0 秒消費）。time_margin_ms = 0
        // 設定なので margin による消費もない。
        assert_eq!(replayed_core.clock_remaining_main_ms(Color::Black), 60_000);
        assert_eq!(replayed_core.clock_remaining_main_ms(Color::White), 60_000);
    }

    #[test]
    fn replay_with_invalid_initial_sfen_returns_invalid_sfen() {
        let mut cfg = baseline_config();
        cfg.initial_sfen = Some("totally-broken-sfen".to_owned());
        let summary = replay_core_room(&cfg, &[]);
        let ReplaySummary::InvalidSfen { reason } = summary else {
            panic!("expected InvalidSfen, got {summary:?}");
        };
        // `reason` は console_log で運用が原因を特定するための情報源。空にならない
        // 契約をここで固定する（空文字だと wrangler tail で何が壊れたか分からない）。
        assert!(!reason.is_empty(), "reason must be non-empty for diagnostics");
    }

    #[test]
    fn replay_with_unknown_color_in_move_row_returns_unknown_color() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        let moves = vec![move_row(1, "purple", "+7776FU,T3", 3_000)];
        let summary = replay_core_room(&cfg, &moves);
        let ReplaySummary::UnknownColor { ply, color } = summary else {
            panic!("expected UnknownColor, got {summary:?}");
        };
        assert_eq!(ply, 1);
        assert_eq!(color, "purple");
    }

    #[test]
    fn replay_with_invalid_move_returns_move_replay_failed() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        // 黒の番に white が動く CSA 行。手番外なので handle_line で reject される。
        let moves = vec![move_row(1, "white", "-3334FU,T2", 3_000)];
        let summary = replay_core_room(&cfg, &moves);
        let ReplaySummary::MoveReplayFailed { ply, line, reason } = summary else {
            panic!("expected MoveReplayFailed, got {summary:?}");
        };
        assert_eq!(ply, 1);
        assert_eq!(line, "-3334FU,T2");
        assert!(!reason.is_empty(), "reason must be non-empty for diagnostics");
    }

    /// `FinishedState` は終局確定後に DO storage へ書き戻される。schema 拡張で
    /// フィールドを増やす際に既存のシリアライズ表現と齟齬が起きないことを
    /// round-trip で固定する。
    #[test]
    fn finished_state_round_trips_through_serde_json() {
        let original = FinishedState {
            result_code: "#RESIGN".to_owned(),
            ended_at_ms: PLAY_STARTED_AT_MS + 9_000,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: FinishedState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.result_code, original.result_code);
        assert_eq!(restored.ended_at_ms, original.ended_at_ms);
    }

    #[test]
    fn replay_with_buoy_initial_sfen_preserves_starting_position() {
        let mut cfg = baseline_config();
        // 平手以外の局面（白番開始の中盤局面）を SFEN として与え、replay 後の盤面が
        // SFEN と一致することを確認する。`%%FORK` / buoy 経由の対局に相当。
        let buoy_sfen = "lnsg1gsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1";
        cfg.initial_sfen = Some(buoy_sfen.to_owned());
        let summary = replay_core_room(&cfg, &[]);
        let ReplaySummary::Restored { core } = summary else {
            panic!("expected Restored, got {summary:?}");
        };
        // SFEN ラウンドトリップで開始局面が保たれること。`current_turn` も白で一致する。
        assert_eq!(core.position().to_sfen(), buoy_sfen);
        assert_eq!(core.current_turn(), Color::White);
    }
}
