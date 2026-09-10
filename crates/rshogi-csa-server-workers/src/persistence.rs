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
//! - `reconnect_clock_credits` テーブル: ply ごとの有界な再接続時計補償（SQL）
//! - [`FinishedState`]: 終局確定後のフラグ（同 DO で再起動した場合の早期 return）
//!
//! cold start 復元の手順は [`replay_core_room`] のドキュメントを参照。

use crate::attachment::FinalizingBroadcast;
use serde::{Deserialize, Serialize};

use rshogi_csa_server::ClockSpec;
use rshogi_csa_server::game::room::{
    GameRoom as CoreRoom, GameRoomConfig, HandleOutcome, HandleResult,
};
use rshogi_csa_server::types::{Color, CsaLine, GameId, PlayerName};

/// マッチ成立時に永続化する対局設定。`CoreRoom` の再構築に必要な最小情報。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedConfig {
    /// 対局 ID。`<room_id>-<epoch_ms>` 形式で `start_match` が生成する。
    pub(crate) game_id: String,
    /// 先手プレイヤのハンドル。
    pub(crate) black_handle: String,
    /// 先手の公開用 opaque player ID。旧 snapshot は空文字から legacy ID に解決する。
    #[serde(default)]
    pub(crate) black_player_id: String,
    /// 後手プレイヤのハンドル。
    pub(crate) white_handle: String,
    /// 後手の公開用 opaque player ID。旧 snapshot は空文字から legacy ID に解決する。
    #[serde(default)]
    pub(crate) white_player_id: String,
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
    /// 対局開始時に確定した再接続 grace 時間（ミリ秒）。
    ///
    /// `websocket_close` 時に env を読み直すと、対局中の deploy / 設定変更で
    /// token 配布有無と grace 判定がずれるため、マッチ単位で固定した値を保存する。
    /// 旧 schema では存在しないため `None` として読み、保守的に grace 無効として扱う。
    #[serde(default)]
    pub(crate) reconnect_grace_ms: Option<u64>,
    /// 先手向けに発行した再接続トークン。`Game_Summary` 末尾拡張行で配布した
    /// 値そのまま (32 文字 hex)。`websocket_close` 時に grace registry へ写して
    /// 切断側 LOGIN reconnect 要求の `expected_token` 照合に使う。再接続プロトコル
    /// を有効化していない構成では `None`。`#[serde(default)]` を付けているのは、
    /// 旧 schema (本フィールド導入前) で永続化された snapshot からの cold start
    /// で deserialize 失敗を起こさないため。
    #[serde(default)]
    pub(crate) black_reconnect_token: Option<String>,
    /// 後手向けの再接続トークン。挙動・契約は [`Self::black_reconnect_token`] と同様。
    #[serde(default)]
    pub(crate) white_reconnect_token: Option<String>,
    /// 対局開始時に確定した `%KACHI` 判定の入玉ルール (USI トークン、例 `CSARule27`)。
    ///
    /// env `ENTERING_KING_RULE` を対局中に変えても進行中の局が別ルールで判定されない
    /// よう、マッチ単位で固定した値を保存する ([`Self::reconnect_grace_ms`] と同じ理由)。
    /// 旧 schema には存在しないため `#[serde(default)]` で `None` として読み、
    /// [`Self::entering_king_rule`] が [`crate::config::DEFAULT_ENTERING_KING_RULE`] に
    /// フォールバックする。
    #[serde(default)]
    pub(crate) entering_king_rule: Option<String>,
}

impl PersistedConfig {
    /// 保存した USI トークンから `%KACHI` 判定ルールを復元する。`None` / 不正
    /// トークン (旧 schema・手書き改変) は [`crate::config::DEFAULT_ENTERING_KING_RULE`]。
    pub(crate) fn entering_king_rule(&self) -> rshogi_core::types::EnteringKingRule {
        self.entering_king_rule
            .as_deref()
            .and_then(rshogi_core::types::EnteringKingRule::from_usi)
            .unwrap_or(crate::config::DEFAULT_ENTERING_KING_RULE)
    }
}

/// 終局フラグ。一度 `Some` になったらその DO は同じ対局を二度開始しない。
///
/// `exported_at_ms` は R2 棋譜エクスポートが全 PUT 完了した時刻 (UNIX epoch ms)。
/// `None` のあいだは終局はしているが R2 への書き出しが未完了で、R2 export retry の
/// `KEY_EXPORT_PENDING` + `PendingAlarmKind::ExportRetry` 経路で再試行されている
/// ことを示す。`#[serde(default)]` で旧 schema (このフィールド導入前の cold start
/// snapshot) との互換を保つ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishedState {
    /// CSA 終局コード（`#RESIGN` / `#TIME_UP` / `#ILLEGAL_MOVE` 等）。
    pub(crate) result_code: String,
    /// 終局確定時刻（UNIX エポック ミリ秒）。
    pub(crate) ended_at_ms: u64,
    /// R2 棋譜エクスポート完了時刻。`None` は export 未完了 (retry 待ち) を示す。
    #[serde(default)]
    pub(crate) exported_at_ms: Option<u64>,
}

/// 終局時の R2 export PUT のうち失敗した key を表すエントリ (R2 export retry)。
///
/// `body_kind` で再 PUT 時に乗せる本文 (`csa` = CSA 本文 / `meta` = JSON meta) を
/// 区別する。`key` 文字列の prefix 推測ではなく明示分類にするのは、key 形式の
/// 将来変更に対する堅牢性のため (例: `kifu-by-id/` と `games-index/` で同 JSON
/// body だが path が違う、`YYYY/MM/DD/` と `kifu-by-id/` で同 CSA 本文だが path
/// が違う、といった対応関係を struct で固定する)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedExportObject {
    /// R2 オブジェクトキー (例: `2026/05/08/g1.csa` / `kifu-by-id/g1.meta.json`)。
    pub(crate) key: String,
    /// 再 PUT 時に本文として乗せる種別。
    pub(crate) body_kind: ExportBodyKind,
}

/// `FailedExportObject` の本文種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportBodyKind {
    /// CSA V2 形式の棋譜本文 (`YYYY/MM/DD/<id>.csa` / `kifu-by-id/<id>.csa`)。
    Csa,
    /// `GamesIndexEntry` を serialize した JSON (`kifu-by-id/<id>.meta.json` /
    /// `games-index/<inv>-<id>.json`)。
    Meta,
}

/// 終局時に R2 export の一部または全部が失敗したときに DO storage へ残す
/// retry payload。
///
/// CSA 本文 / meta JSON は finalize 時点で計算済のものをそのまま保存する
/// (cold start 後でも `load_moves` を呼び直さず再 PUT できる)。`failed_keys`
/// は初回 finalize 時点で実際に PUT 失敗した key のみが残り、retry alarm が
/// 順番に再 PUT する。`attempt` は再試行回数で、`RETRY_DELAYS_SEC` の上限を
/// 超えたら exhausted として alarm を停止する (= pending entry は残置し
/// 観測性を保つ)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPendingState {
    /// 対局 ID。観測ログ / cold start 後の retry 経路の primary key。
    pub(crate) game_id: String,
    /// 終局確定時刻。`exported_at_ms` を埋める際に再利用する。
    pub(crate) ended_at_ms: u64,
    /// 再 PUT 用の CSA V2 棋譜本文 (`Csa` 種別の `failed_keys` に流す)。
    pub(crate) csa_text: String,
    /// 再 PUT 用の meta JSON 本文 (`Meta` 種別の `failed_keys` に流す)。
    pub(crate) meta_body: Vec<u8>,
    /// 残っている再 PUT 対象 key 一覧 (初回 finalize で失敗したもの)。
    pub(crate) failed_keys: Vec<FailedExportObject>,
    /// これまでの retry 回数 (0 = 初回 finalize 後の最初の alarm が attempt=0)。
    pub(crate) attempt: u32,
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
    /// この手番で再接続により課金対象外とした累積時間（ミリ秒）。
    /// `moves` と `reconnect_clock_credits` の LEFT JOIN で埋め、過去の補償手も
    /// cold-start replay 時に同じ `T` / 時計消費へ復元する。
    #[serde(default)]
    pub(crate) reconnect_credit_ms: i64,
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
        /// 復元済み `CoreRoom`。終局手まで保存済みなら `Finished` となるため、
        /// 呼び出し側は未完了の終局永続化を再開する。
        core: Box<CoreRoom>,
        /// 再開時にも終局手の告知を失わないよう保持する。
        last_result: Option<HandleResult>,
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
///    - `moves` を ply 順に逐次 `handle_line` で再生する。各手の直前にその ply の
///      `reconnect_credit_ms` を戻し、wall-clock タイムスタンプは
///      `at_ms.max(0).max(t)` に正規化（負値や AGREE より前の `at_ms` は clock を
///      巻き戻すため）
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
            entering_king_rule: cfg.entering_king_rule(),
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
            last_result: None,
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

    let mut last_result = None;
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
        core.restore_current_turn_reconnect_credit_ms(
            m.reconnect_credit_ms.max(0) as u64,
            cfg.clock.reconnect_compensation_limit_ms(),
        );
        let ts = (m.at_ms.max(0) as u64).max(play_started_at_ms);
        match core.handle_line(color, &CsaLine::new(&m.line), ts) {
            Ok(result) => last_result = Some(result),
            Err(e) => {
                return ReplaySummary::MoveReplayFailed {
                    ply: m.ply,
                    line: m.line.clone(),
                    reason: format!("{e:?}"),
                };
            }
        }
    }

    ReplaySummary::Restored {
        core: Box::new(core),
        last_result,
    }
}

/// replay が返した告知を保ち、履歴にない裁定を生成しない。
pub fn pending_finalization(result: Option<HandleResult>) -> Option<HandleResult> {
    result.filter(|result| matches!(result.outcome, HandleOutcome::GameEnded(_)))
}

/// 告知前に保存する終局の裁定と終局時刻。isolate 破棄後の再開と再試行で
/// 同じ裁定・同じ終局時刻 (R2 のキーと本文) を使うために残す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalizingState {
    /// 確定させる裁定。
    pub result: rshogi_csa_server::game::result::GameResult,
    /// 棋譜・履歴に記録する終局時刻 (epoch ms)。
    pub ended_at_ms: u64,
    /// 復帰後も終局手と時計を元の順序で告知するために残す。
    pub broadcasts: Vec<FinalizingBroadcast>,
    /// 外部イベントの頻度で再試行の猶予を消費しないため、初回と alarm のみ数える。
    pub attempt: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_csa_server::GameStatus;
    use rshogi_csa_server::game::result::{GameResult, IllegalReason};

    #[test]
    fn finalizing_state_roundtrips_every_result() {
        for result in [
            GameResult::Toryo {
                winner: Color::Black,
            },
            GameResult::TimeUp {
                loser: Color::White,
            },
            GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::IllegalKachi,
            },
            GameResult::Kachi {
                winner: Color::White,
            },
            GameResult::OuteSennichite {
                loser: Color::Black,
            },
            GameResult::Sennichite,
            GameResult::MaxMoves,
            GameResult::Abnormal { winner: None },
        ] {
            let state = FinalizingState {
                broadcasts: vec![FinalizingBroadcast {
                    target: rshogi_csa_server::BroadcastTarget::All,
                    line: "-5251OU,T3".into(),
                    ply: Some(12),
                }],
                attempt: 3,
                result,
                ended_at_ms: 1_700_000_000_000,
            };
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(serde_json::from_str::<FinalizingState>(&json).unwrap(), state);
        }
    }
    use rshogi_csa_server::record::kifu::primary_result_code;

    /// `play_started_at_ms` の代表値（適当な epoch ms）。テスト全体で共有。
    const PLAY_STARTED_AT_MS: u64 = 1_000_000;

    fn baseline_config() -> PersistedConfig {
        PersistedConfig {
            game_id: "room-1-test".to_owned(),
            black_handle: "alice".to_owned(),
            black_player_id: crate::player_identity::legacy_player_id("alice"),
            white_handle: "bob".to_owned(),
            white_player_id: crate::player_identity::legacy_player_id("bob"),
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
            reconnect_grace_ms: Some(0),
            black_reconnect_token: None,
            white_reconnect_token: None,
            entering_king_rule: None,
        }
    }

    fn move_row(ply: i64, color: &str, line: &str, at_ms_offset_from_start: u64) -> MoveRow {
        MoveRow {
            ply,
            color: color.to_owned(),
            line: line.to_owned(),
            at_ms: (PLAY_STARTED_AT_MS + at_ms_offset_from_start) as i64,
            reconnect_credit_ms: 0,
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
                entering_king_rule: cfg.entering_king_rule(),
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
            core.restore_current_turn_reconnect_credit_ms(
                m.reconnect_credit_ms.max(0) as u64,
                cfg.clock.reconnect_compensation_limit_ms(),
            );
            let ts = (m.at_ms.max(0) as u64).max(t0);
            core.handle_line(color, &CsaLine::new(&m.line), ts)
                .expect("move in directly_played");
        }
        core
    }

    #[test]
    fn entering_king_rule_accessor_maps_token_and_falls_back() {
        use rshogi_core::types::EnteringKingRule;
        let mut cfg = baseline_config();
        // 既定 (None) はフォールバック値。
        assert_eq!(cfg.entering_king_rule(), crate::config::DEFAULT_ENTERING_KING_RULE);
        cfg.entering_king_rule = Some("CSARule24".to_owned());
        assert_eq!(cfg.entering_king_rule(), EnteringKingRule::Point24);
        cfg.entering_king_rule = Some("bogus".to_owned());
        assert_eq!(cfg.entering_king_rule(), crate::config::DEFAULT_ENTERING_KING_RULE);
    }

    #[test]
    fn old_snapshot_without_rule_field_deserializes_to_default() {
        // 本フィールド導入前に永続化された snapshot (entering_king_rule 欠落) を
        // deserialize しても失敗せず、accessor は既定へフォールバックする。
        let mut cfg = baseline_config();
        cfg.entering_king_rule = Some("CSARule24".to_owned());
        let mut json: serde_json::Value = serde_json::to_value(&cfg).unwrap();
        json.as_object_mut().unwrap().remove("entering_king_rule");
        let restored: PersistedConfig = serde_json::from_value(json).unwrap();
        assert_eq!(restored.entering_king_rule, None);
        assert_eq!(restored.entering_king_rule(), crate::config::DEFAULT_ENTERING_KING_RULE);
    }

    #[test]
    fn persisted_rule_survives_serde_round_trip() {
        let mut cfg = baseline_config();
        cfg.entering_king_rule = Some("CSARule24".to_owned());
        let round: PersistedConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(round.entering_king_rule(), rshogi_core::types::EnteringKingRule::Point24);
    }

    #[test]
    fn replay_preserves_full_repetition_history_and_terminal_broadcasts() {
        let long_cycle = [
            "+5949OU", "-5141OU", "+4939OU", "-4131OU", "+3938OU", "-3132OU", "+3837OU", "-3233OU",
            "+3747OU", "-3343OU", "+4757OU", "-4353OU", "+5767OU", "-5363OU", "+6768OU", "-6362OU",
            "+6869OU", "-6261OU", "+6959OU", "-6151OU",
        ];
        for (sfen, cycle, code) in [
            ("4k4/9/9/9/9/9/9/9/4K4 b Pp 1", long_cycle.as_slice(), "#SENNICHITE"),
            (
                "9/6k2/9/9/9/9/9/6R2/K8 w - 1",
                ["-3242OU", "+3848HI", "-4232OU", "+4838HI"].as_slice(),
                "#OUTE_SENNICHITE",
            ),
            (
                "9/5k3/9/9/9/9/9/6R2/K8 b - 1",
                ["+3848HI", "-4232OU", "+4838HI", "-3242OU"].as_slice(),
                "#OUTE_SENNICHITE",
            ),
        ] {
            let mut cfg = baseline_config();
            cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
            cfg.initial_sfen = Some(sfen.to_owned());
            let cfg: PersistedConfig =
                serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
            let rows: Vec<_> = cycle
                .iter()
                .cycle()
                .take(cycle.len() * 3)
                .enumerate()
                .map(|(ply, line)| {
                    move_row(
                        ply as i64 + 1,
                        if line.starts_with('+') {
                            "black"
                        } else {
                            "white"
                        },
                        line,
                        0,
                    )
                })
                .collect();
            let rows: Vec<MoveRow> =
                serde_json::from_str(&serde_json::to_string(&rows).unwrap()).unwrap();
            let mut uninterrupted = directly_played(&cfg, &[]);
            let split = rows.len() - 1;
            for row in &rows[..split] {
                let side = if row.color == "black" {
                    Color::Black
                } else {
                    Color::White
                };
                let result = uninterrupted
                    .handle_line(side, &CsaLine::new(&row.line), PLAY_STARTED_AT_MS)
                    .unwrap();
                assert!(matches!(result.outcome, HandleOutcome::MoveAccepted { .. }));
            }
            let ReplaySummary::Restored { mut core, .. } = replay_core_room(&cfg, &rows[..split])
            else {
                panic!("restore failed");
            };
            assert!(!matches!(core.status(), GameStatus::Finished(_)));
            let last = &rows[split];
            let side = if last.color == "black" {
                Color::Black
            } else {
                Color::White
            };
            let expected = uninterrupted
                .handle_line(side, &CsaLine::new(&last.line), PLAY_STARTED_AT_MS)
                .unwrap();
            let actual =
                core.handle_line(side, &CsaLine::new(&last.line), PLAY_STARTED_AT_MS).unwrap();
            assert_eq!(actual.outcome, expected.outcome);
            if code == "#SENNICHITE" {
                assert!(matches!(actual.outcome, HandleOutcome::GameEnded(GameResult::Sennichite)));
                assert!(actual.broadcasts.iter().any(|b| b.line.as_str() == "#DRAW"));
            } else {
                assert!(matches!(
                    actual.outcome,
                    HandleOutcome::GameEnded(GameResult::OuteSennichite {
                        loser: Color::Black
                    })
                ));
                assert!(actual.broadcasts.iter().any(|b| b.line.as_str() == "#LOSE"));
                assert!(actual.broadcasts.iter().any(|b| b.line.as_str() == "#WIN"));
            }
            assert!(actual.broadcasts.iter().any(|b| b.line.as_str() == code));
            assert!(actual.broadcasts.iter().any(
                |b| b.ply == Some(rows.len() as u32) && b.line.as_str().starts_with(&last.line)
            ));
            let messages = |result: &rshogi_csa_server::game::room::HandleResult| {
                result
                    .broadcasts
                    .iter()
                    .map(|b| (b.target, b.line.as_str().to_owned(), b.ply))
                    .collect::<Vec<_>>()
            };
            assert_eq!(messages(&actual), messages(&expected));
            let ReplaySummary::Restored {
                core: finished,
                last_result,
            } = replay_core_room(&cfg, &rows)
            else {
                panic!("terminal move restore failed");
            };
            assert_eq!(finished.status(), core.status());
            assert_eq!(finished.moves_played(), rows.len() as u32);
            let pending = pending_finalization(last_result).expect("終局の永続化が必要");
            assert_eq!(pending.outcome, expected.outcome);
            assert_eq!(pending.broadcasts, expected.broadcasts);
        }
    }

    #[test]
    fn pending_finalization_preserves_max_moves_after_terminal_move_replay() {
        let mut cfg = baseline_config();
        assert!(pending_finalization(None).is_none());
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        cfg.max_moves = 1;
        let rows = [move_row(1, "black", "+7776FU", 0)];
        let ReplaySummary::Restored {
            mut core,
            last_result,
        } = replay_core_room(&cfg, &rows)
        else {
            panic!("terminal move restore failed");
        };
        assert_eq!(core.force_time_up(Color::White).outcome, HandleOutcome::Continue);
        let pending = pending_finalization(last_result).expect("終局の永続化が必要");
        assert_eq!(pending.outcome, HandleOutcome::GameEnded(GameResult::MaxMoves));
        assert!(pending.broadcasts.iter().any(|entry| entry.ply == Some(1)));
        assert_eq!(core.moves_played(), 1);
    }

    #[test]
    fn replay_without_play_started_returns_agree_waiting_room() {
        let cfg = baseline_config();
        let summary = replay_core_room(&cfg, &[]);
        let ReplaySummary::Restored { core, .. } = summary else {
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
        let ReplaySummary::Restored { core, .. } = summary else {
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
            ..
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

    #[test]
    fn replay_reapplies_credit_for_past_reconnected_turns() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        cfg.clock = ClockSpec::Countdown {
            total_time_sec: 1,
            byoyomi_sec: 1,
        };
        let mut first = move_row(1, "black", "+7776FU,T2", 3_500);
        first.reconnect_credit_ms = 1_000;
        let second = move_row(2, "white", "-3334FU,T0", 4_000);
        let moves = vec![first.clone(), second.clone()];

        let ReplaySummary::Restored { core, .. } = replay_core_room(&cfg, &moves) else {
            panic!("credit を含む過去手は replay できる必要がある");
        };
        assert_eq!(core.moves_played(), 2);
        assert_eq!(core.current_turn(), Color::Black);
        assert_eq!(core.clock_remaining_main_ms(Color::Black), 0);
        assert_eq!(core.clock_remaining_main_ms(Color::White), 1_000);

        // 同じ wall-clock 列で credit が無いと先手 1 手目が TimeUp になり、後手を
        // replay できない。テストが credit 適用を実際に必要としていることを固定する。
        let mut without_credit = moves;
        without_credit[0].reconnect_credit_ms = 0;
        assert!(matches!(
            replay_core_room(&cfg, &without_credit),
            ReplaySummary::MoveReplayFailed { ply: 2, .. }
        ));
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
                ..
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
            ..
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
            ..
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
            ..
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

    /// hibernation evict を跨いだ cold start で replay 直後の `turn_started_at_ms`
    /// は最後の手の at_ms のまま残る (#852 の計時統一)。DO 側は起点を「今」へ
    /// 張り直さないため、休眠滞留を跨いで実時間が予算を超えていれば、live 側の次の
    /// 手でも replay 同様に `TimeUp` する。live と replay が同じ規則で課金される
    /// (= 乖離しない) ことを固定する対照テスト
    /// (<https://github.com/SH11235/rshogi/issues/852>)。
    #[test]
    fn cold_start_charges_real_elapsed_and_times_out_when_over_budget() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        let moves = vec![move_row(1, "black", "+7776FU,T3", 3_000)];
        let ReplaySummary::Restored { mut core, .. } = replay_core_room(&cfg, &moves) else {
            panic!("expected Restored");
        };
        // 黒の手 (at_ms = PLAY_STARTED + 3s) の 30 分後に白が指す（白番の長考中に DO
        // が evict され、復帰後に着手した状況）。本体 60s + 秒読み 10s の予算を実時間が
        // 大きく超えるため、live 側 (handle_line) でも TimeUp(White) になる。これが
        // 従来 replay 側だけが TimeUp し live と乖離していた幽霊 TimeUp を解消する。
        let resumed_at_ms = PLAY_STARTED_AT_MS + 30 * 60_000;
        let result = core
            .handle_line(Color::White, &CsaLine::new("-3334FU,T1"), resumed_at_ms + 1_000)
            .expect("handle_line itself must succeed");
        assert!(
            matches!(
                result.outcome,
                HandleOutcome::GameEnded(GameResult::TimeUp {
                    loser: Color::White
                })
            ),
            "実時間が予算超過なら live 側でも TimeUp するはず: {:?}",
            result.outcome
        );
    }

    /// 「大きな at_ms ギャップ (長考) を含む手列」でも各手の実消費が予算内なら、
    /// replay は `MoveReplayFailed` にならず `Restored` する (#852 root fix の対照)。
    /// 本番で発症した fischer-60-5F を模し、40 秒級の長考を 2 手繋いでも 60s バンク +
    /// 5s increment の予算内なので TimeUp しない
    /// (<https://github.com/SH11235/rshogi/issues/852>)。
    #[test]
    fn cold_start_replay_with_large_but_within_budget_fischer_gaps_restores() {
        let mut cfg = baseline_config();
        cfg.clock = ClockSpec::Fischer {
            total_time_sec: 60,
            increment_sec: 5,
        };
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        // 黒 40s 長考 → 白 40s 長考。いずれも 60s バンク + 5s increment の予算内。
        let moves = vec![
            move_row(1, "black", "+7776FU,T40", 40_000),
            move_row(2, "white", "-3334FU,T40", 80_000),
        ];
        let ReplaySummary::Restored { core, .. } = replay_core_room(&cfg, &moves) else {
            panic!("予算内の長考なら Restored のはず (MoveReplayFailed になってはいけない)");
        };
        assert_eq!(core.moves_played(), 2);
        // 予算内なので両者とも残時間が正 (TimeUp していない)。
        assert!(core.clock_remaining_main_ms(Color::Black) > 0);
        assert!(core.clock_remaining_main_ms(Color::White) > 0);
    }

    /// cold-start 復元後の alarm 判定を固定する (#852)。計時起点を張り直さないので、
    /// 実時間が予算を超えた `now` では `current_turn_remaining_ms` が `0` を返し、
    /// alarm は `force_time_up` を発火する。予算内の `now` では残時間が正で、alarm は
    /// 残時間ぶん貼り直す (<https://github.com/SH11235/rshogi/issues/852>)。
    #[test]
    fn cold_start_alarm_decides_time_up_from_real_remaining() {
        let mut cfg = baseline_config();
        cfg.play_started_at_ms = Some(PLAY_STARTED_AT_MS);
        let moves = vec![move_row(1, "black", "+7776FU,T3", 3_000)];
        let ReplaySummary::Restored { mut core, .. } = replay_core_room(&cfg, &moves) else {
            panic!("expected Restored");
        };
        // 次手番は白。計時起点は黒の手の at_ms (= PLAY_STARTED + 3s) のまま。
        let turn_start = PLAY_STARTED_AT_MS + 3_000;
        let budget = core.clock_turn_budget_ms(Color::White).max(0) as u64;
        assert!(budget > 0);
        // 予算内の now → 残時間が正 → alarm は reschedule する。
        let within = turn_start + budget / 2;
        assert_eq!(core.current_turn_remaining_ms(within), Some(budget - budget / 2));
        // 予算超過の now (30 分後) → 残 0 → alarm は force_time_up する。
        let over = turn_start + 30 * 60_000;
        assert_eq!(core.current_turn_remaining_ms(over), Some(0));
        // 実際に force_time_up が GameEnded(TimeUp{White}) を返すこと。
        let result = core.force_time_up(core.current_turn());
        assert!(matches!(
            result.outcome,
            HandleOutcome::GameEnded(GameResult::TimeUp {
                loser: Color::White
            })
        ));
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
                reconnect_credit_ms: 0,
            },
            MoveRow {
                ply: 2,
                color: "white".to_owned(),
                line: "-3334FU,T0".to_owned(),
                at_ms: (PLAY_STARTED_AT_MS - 10_000) as i64,
                reconnect_credit_ms: 0,
            },
        ];
        let ReplaySummary::Restored {
            core: replayed_core,
            ..
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
            exported_at_ms: Some(PLAY_STARTED_AT_MS + 9_500),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: FinishedState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.result_code, original.result_code);
        assert_eq!(restored.ended_at_ms, original.ended_at_ms);
        assert_eq!(restored.exported_at_ms, original.exported_at_ms);
    }

    /// 旧 schema (`exported_at_ms` フィールド導入前) で書かれた DO storage
    /// 値が `#[serde(default)]` 経由で `None` として deserialize できることを
    /// 固定する。この契約が壊れると cold start 後の DO で
    /// `load_finished` が失敗し、終局済 DO が新規 LOGIN を受け付ける退行になる。
    #[test]
    fn finished_state_deserializes_when_exported_at_ms_absent() {
        let json = r##"{
            "result_code": "#RESIGN",
            "ended_at_ms": 1234567890
        }"##;
        let restored: FinishedState = serde_json::from_str(json).unwrap();
        assert_eq!(restored.result_code, "#RESIGN");
        assert_eq!(restored.ended_at_ms, 1_234_567_890);
        assert_eq!(restored.exported_at_ms, None);
    }

    /// `ExportPendingState` の round-trip 固定 (R2 export retry の永続化契約)。
    /// `failed_keys` の `body_kind` が `csa` / `meta` の lower snake で
    /// wire される (rename_all) ことも実装契約として固定する。
    #[test]
    fn export_pending_state_round_trips_through_serde_json() {
        let original = ExportPendingState {
            game_id: "lobby-1".to_owned(),
            ended_at_ms: PLAY_STARTED_AT_MS + 10_000,
            csa_text: "V2.2\nN+alice\nN-bob\n".to_owned(),
            meta_body: br#"{"game_id":"lobby-1"}"#.to_vec(),
            failed_keys: vec![
                FailedExportObject {
                    key: "2026/05/08/lobby-1.csa".to_owned(),
                    body_kind: ExportBodyKind::Csa,
                },
                FailedExportObject {
                    key: "kifu-by-id/lobby-1.meta.json".to_owned(),
                    body_kind: ExportBodyKind::Meta,
                },
            ],
            attempt: 1,
        };
        let json = serde_json::to_string(&original).unwrap();
        // wire 形式で `body_kind` が snake_case の lower 表現になっていること
        assert!(json.contains(r#""body_kind":"csa""#), "json={json}");
        assert!(json.contains(r#""body_kind":"meta""#), "json={json}");
        let restored: ExportPendingState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn replay_with_buoy_initial_sfen_preserves_starting_position() {
        let mut cfg = baseline_config();
        // 平手以外の局面（白番開始の中盤局面）を SFEN として与え、replay 後の盤面が
        // SFEN と一致することを確認する。`%%FORK` / buoy 経由の対局に相当。
        let buoy_sfen = "lnsg1gsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1";
        cfg.initial_sfen = Some(buoy_sfen.to_owned());
        let summary = replay_core_room(&cfg, &[]);
        let ReplaySummary::Restored { core, .. } = summary else {
            panic!("expected Restored, got {summary:?}");
        };
        // SFEN ラウンドトリップで開始局面が保たれること。`current_turn` も白で一致する。
        assert_eq!(core.position().to_sfen(), buoy_sfen);
        assert_eq!(core.current_turn(), Color::White);
    }

    /// 旧 schema (本フィールド導入前の cold start snapshot) に
    /// `black_reconnect_token` / `white_reconnect_token` フィールドが存在しなくても、
    /// `#[serde(default)]` で `None` として deserialize できる。https://github.com/SH11235/rshogi/issues/591 hotfix で
    /// `start_match` が常に `Some` を書く挙動から `None` も書く挙動に変わったため、
    /// 旧 snapshot との互換性を回帰防止する。
    #[test]
    fn persisted_config_deserializes_when_reconnect_token_fields_absent() {
        let json = r#"{
            "game_id": "room-1-old",
            "black_handle": "alice",
            "white_handle": "bob",
            "game_name": "g1",
            "clock": {"kind":"countdown","total_time_sec":60,"byoyomi_sec":10},
            "max_moves": 256,
            "time_margin_ms": 0,
            "matched_at_ms": 999000,
            "play_started_at_ms": null,
            "initial_sfen": null
        }"#;
        let cfg: PersistedConfig =
            serde_json::from_str(json).expect("旧 schema は default 経由で deserialize できる");
        assert_eq!(cfg.reconnect_grace_ms, None);
        assert_eq!(cfg.black_reconnect_token, None);
        assert_eq!(cfg.white_reconnect_token, None);
        assert!(cfg.black_player_id.is_empty());
        assert!(cfg.white_player_id.is_empty());
    }

    /// 明示 `null` 値は `None` として読まれる (`#[serde(default)]` と同等の振る舞い)。
    #[test]
    fn persisted_config_deserializes_null_reconnect_token_as_none() {
        let json = r#"{
            "game_id": "room-1-null",
            "black_handle": "alice",
            "white_handle": "bob",
            "game_name": "g1",
            "clock": {"kind":"countdown","total_time_sec":60,"byoyomi_sec":10},
            "max_moves": 256,
            "time_margin_ms": 0,
            "matched_at_ms": 999000,
            "play_started_at_ms": null,
            "initial_sfen": null,
            "reconnect_grace_ms": null,
            "black_reconnect_token": null,
            "white_reconnect_token": null
        }"#;
        let cfg: PersistedConfig =
            serde_json::from_str(json).expect("null 値は None として deserialize できる");
        assert_eq!(cfg.reconnect_grace_ms, None);
        assert_eq!(cfg.black_reconnect_token, None);
        assert_eq!(cfg.white_reconnect_token, None);
    }

    /// 値あり (`grace > 0` で `start_match` が token を発行した場合の永続化形式) も
    /// そのまま読み込める。grace 値と token 値の round-trip を pin する。
    #[test]
    fn persisted_config_round_trips_with_reconnect_token_values() {
        let mut original = baseline_config();
        original.reconnect_grace_ms = Some(30_000);
        original.black_reconnect_token = Some("a".repeat(32));
        original.white_reconnect_token = Some("b".repeat(32));
        let json = serde_json::to_string(&original).expect("serialize cfg");
        let restored: PersistedConfig =
            serde_json::from_str(&json).expect("deserialize cfg with token values");
        assert_eq!(restored.reconnect_grace_ms, original.reconnect_grace_ms);
        assert_eq!(restored.black_reconnect_token, original.black_reconnect_token);
        assert_eq!(restored.white_reconnect_token, original.white_reconnect_token);
    }
}
