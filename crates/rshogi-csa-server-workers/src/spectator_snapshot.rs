//! 観戦者向け snapshot の構築（純粋関数）。
//!
//! `%%MONITOR2ON <gameId>` 受理時に、対局の現状を CSA wire に流すための行列を
//! 組み立てる。本モジュールは I/O を持たず DO state にも依存しないため、ホスト
//! target の単体テストで wire 出力を pin する。
//!
//! wire 順序 (`build_spectator_snapshot` の戻り値):
//!
//! 1. 観戦者向け `BEGIN Game_Summary` ブロック（`Black/White_Time_Remaining_Ms:`
//!    末尾拡張行を含む、player 経路の `Your_Turn:` / `Reconnect_Token:` は含まない）
//! 2. これまでの move 行 (1 行 1 手): `<token>,T<elapsed_sec>` 形式
//!    （broadcast の通常形式と一致）
//! 3. （終局済 DO の場合のみ）終局結果コード行 (`#RESIGN` / `#TIME_UP` 等)
//!
//! `BEGIN Position` / `END Position` は Game_Summary の `position_section` 内部に
//! 含まれており、本モジュールが別途出力することはない。
//!
//! クライアント側は `##[MONITOR2] BEGIN <id>` と `##[MONITOR2] END` の間で本関数の
//! 戻り値を順次受信し、`END` 受信を hard delimiter として state を全置換する。

use rshogi_csa_server::protocol::summary::{
    GameSummaryBuilder, position_section_from_sfen, side_to_move_from_sfen,
    standard_initial_position_block,
};
use rshogi_csa_server::types::{Color, GameId, PlayerName};

use crate::persistence::{FinishedState, MoveRow, PersistedConfig};

/// 観戦者用の残り時間スナップショット。
///
/// `core.clock_remaining_main_ms(Color)` と `CoreRoom::current_turn()` から構築
/// する純粋データで、storage には永続化しない。`Color` は
/// `rshogi_csa_server::types::Color` を使う (`rshogi_csa_server` crate を直接
/// 参照する点に注意)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpectatorClocks {
    /// 先手の本体残時間 (ms 粒度、秒読みは含まない)。
    pub black_remaining_ms: u64,
    /// 後手の本体残時間 (ms 粒度、秒読みは含まない)。
    pub white_remaining_ms: u64,
    /// wire 上は手番側を示す。`CoreRoom::current_turn()` の戻り値をそのまま
    /// 入れる契約 (`SpectatorClocks::side_to_move` は wire 上の意味を表す
    /// field 名で、source 側は `current_turn()`)。
    pub side_to_move: Color,
}

/// `build_spectator_snapshot` への入力。
pub struct SpectatorSnapshotInput<'a> {
    /// 永続化済み対局設定（クロック設定 / 初期 SFEN / プレイヤ名 / game_id 等）。
    pub config: &'a PersistedConfig,
    /// `moves` テーブルを ply 昇順で読み出した結果。空なら初手前。
    pub moves: &'a [MoveRow],
    /// snapshot 取得時点の clock 残時間スナップショット (= `ensure_core_loaded`
    /// 直後に `CoreRoom` から取得した値)。
    pub clocks: &'a SpectatorClocks,
    /// 終局済の場合のみ `Some`。snapshot 末尾に `result_code` 行を 1 行追加する。
    pub finalized: Option<&'a FinishedState>,
}

/// 観戦者向け snapshot の wire 行を組み立てる純粋関数。
///
/// 戻り値は CSA 行の `Vec<String>`。各行は末尾改行を含まないため、呼び出し側
/// (DO 側 `send_line`) で改行を付与する契約。
pub fn build_spectator_snapshot(input: SpectatorSnapshotInput<'_>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    let position_section = match input.config.initial_sfen.as_deref() {
        Some(sfen) => position_section_from_sfen(sfen).unwrap_or_else(|_| {
            // SFEN 不正は本来 `start_match` で検出済みのはずだが、永続化レイヤから
            // 想定外の SFEN が読み出された場合の安全側フォールバックとして平手
            // ブロックを返す。観戦者は state 全置換のため、この場合でも UI は
            // 平手で復元できる。
            standard_initial_position_block()
        }),
        None => standard_initial_position_block(),
    };

    let to_move = match input.config.initial_sfen.as_deref() {
        Some(sfen) => side_to_move_from_sfen(sfen).unwrap_or(Color::Black),
        None => Color::Black,
    };

    let time_section = input.config.clock.format_time_section();

    let builder = GameSummaryBuilder {
        game_id: GameId::new(input.config.game_id.clone()),
        black: PlayerName::new(input.config.black_handle.clone()),
        white: PlayerName::new(input.config.white_handle.clone()),
        time_section,
        position_section,
        rematch_on_draw: false,
        to_move,
        declaration: String::new(),
        // 観戦者向け builder は token を出力しないため、`None` 固定で渡す
        // (関数内部でも player 経路と異なり token 行は出さない契約)。
        black_reconnect_token: None,
        white_reconnect_token: None,
    };

    let summary = builder
        .build_for_spectator(input.clocks.black_remaining_ms, input.clocks.white_remaining_ms);
    // build_for_spectator は内部で複数行を改行区切りで返すため、行単位に分解して
    // 末尾改行を取り除いた個別行として lines に追加する。
    for raw_line in summary.lines() {
        lines.push(raw_line.to_owned());
    }

    // 既存の指し手 (broadcast move 行と完全に同一書式)。`MoveRow::line` は
    // `+7776FU,T3` のような raw CSA 行をそのまま保持しているため、改行除去
    // だけ済ませて push する。
    for m in input.moves {
        let trimmed = m.line.trim_end_matches(['\r', '\n']);
        lines.push(trimmed.to_owned());
    }

    // 終局済 DO の場合は最終結果コード行を追加。終局時に CoreRoom 側で broadcast
    // した詳細メッセージ (`#WIN` / `#LOSE` 等) は永続化していないため、ここで
    // 復元するのは集約済の `result_code` のみ。client 側は `#RESIGN` / `#TIME_UP`
    // 等を見て onEnd 経路に乗る。
    if let Some(state) = input.finalized {
        lines.push(state.result_code.clone());
    }

    lines
}

#[cfg(test)]
mod tests {
    use rshogi_csa_server::ClockSpec;

    use super::*;

    fn baseline_config() -> PersistedConfig {
        PersistedConfig {
            game_id: "room-1-test".to_owned(),
            black_handle: "alice".to_owned(),
            white_handle: "bob".to_owned(),
            game_name: "g1".to_owned(),
            clock: ClockSpec::Countdown {
                total_time_sec: 600,
                byoyomi_sec: 10,
            },
            max_moves: 256,
            time_margin_ms: 0,
            matched_at_ms: 1_000_000,
            play_started_at_ms: Some(1_000_000),
            initial_sfen: None,
            reconnect_grace_ms: Some(30_000),
            black_reconnect_token: Some("blk-token".to_owned()),
            white_reconnect_token: Some("wht-token".to_owned()),
        }
    }

    fn move_row(ply: i64, color: &str, line: &str) -> MoveRow {
        MoveRow {
            ply,
            color: color.to_owned(),
            line: line.to_owned(),
            at_ms: 1_000_000 + ply * 1_000,
        }
    }

    fn clocks(black: u64, white: u64, side: Color) -> SpectatorClocks {
        SpectatorClocks {
            black_remaining_ms: black,
            white_remaining_ms: white,
            side_to_move: side,
        }
    }

    /// シナリオ 1: 初手前 (= moves 空、終局なし)。
    #[test]
    fn snapshot_before_first_move_emits_summary_only() {
        let cfg = baseline_config();
        let cl = clocks(600_000, 600_000, Color::Black);
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &[],
            clocks: &cl,
            finalized: None,
        });

        // Game_Summary block の始終端と残時間行・初期局面行が含まれる。
        assert!(
            lines.contains(&"BEGIN Game_Summary".to_owned()),
            "missing BEGIN Game_Summary: {lines:?}"
        );
        assert!(
            lines.contains(&"END Game_Summary".to_owned()),
            "missing END Game_Summary: {lines:?}"
        );
        assert!(
            lines.contains(&"Black_Time_Remaining_Ms:600000".to_owned()),
            "missing black remaining: {lines:?}"
        );
        assert!(
            lines.contains(&"White_Time_Remaining_Ms:600000".to_owned()),
            "missing white remaining: {lines:?}"
        );
        // player 専用フィールドが漏れていない。
        assert!(
            !lines.iter().any(|l| l.starts_with("Your_Turn:")),
            "spectator must not emit Your_Turn: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("Reconnect_Token:")),
            "spectator must not leak Reconnect_Token: {lines:?}"
        );
        // 終局行は出ない。
        assert!(
            !lines.iter().any(|l| l.starts_with('#')),
            "no result code line expected: {lines:?}"
        );
    }

    /// シナリオ 2: 数手後 (= moves 3 件、進行中)。
    #[test]
    fn snapshot_after_three_moves_appends_move_lines_in_order() {
        let cfg = baseline_config();
        let moves = vec![
            move_row(1, "black", "+7776FU,T3"),
            move_row(2, "white", "-3334FU,T2"),
            move_row(3, "black", "+8833UM,T4"),
        ];
        let cl = clocks(597_000, 598_000, Color::White);
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &moves,
            clocks: &cl,
            finalized: None,
        });

        // 全 move 行が順序通り含まれる。
        let move_indices: Vec<usize> = ["+7776FU,T3", "-3334FU,T2", "+8833UM,T4"]
            .iter()
            .map(|m| {
                lines
                    .iter()
                    .position(|l| l == m)
                    .unwrap_or_else(|| panic!("missing move line {m}: {lines:?}"))
            })
            .collect();
        assert!(
            move_indices.windows(2).all(|w| w[0] < w[1]),
            "move lines must be ply-ascending: {move_indices:?}"
        );
        // 全 move 行は END Game_Summary より後に来る。
        let end_idx = lines.iter().position(|l| l == "END Game_Summary").unwrap();
        assert!(move_indices.iter().all(|&i| i > end_idx));
        // 終局行は出ない。
        assert!(!lines.iter().any(|l| l.starts_with('#')));
    }

    /// シナリオ 3: 終局直後 (= moves に %TORYO が含まれる + finalized も Some)。
    /// 一見すると move 行重複 (`%TORYO`) と result code (`#RESIGN`) の二重記録に
    /// 見えるが、クライアントは onEnd を `#RESIGN` 等で確定する仕様で、`%TORYO`
    /// は通常の move stream と同じ位置で再生されるだけ (UI 側は idempotent)。
    #[test]
    fn snapshot_after_toryo_with_finalized_appends_result_code_line() {
        let cfg = baseline_config();
        let moves = vec![
            move_row(1, "black", "+7776FU,T3"),
            move_row(2, "white", "-3334FU,T2"),
            move_row(3, "black", "%TORYO,T1"),
        ];
        let cl = clocks(596_000, 598_000, Color::Black);
        let finished = FinishedState {
            result_code: "#RESIGN".to_owned(),
            ended_at_ms: 1_010_000,
        };
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &moves,
            clocks: &cl,
            finalized: Some(&finished),
        });

        // `%TORYO` 行と `#RESIGN` 行が両方入り、`#RESIGN` は最終位置。
        let toryo_idx = lines
            .iter()
            .position(|l| l == "%TORYO,T1")
            .unwrap_or_else(|| panic!("missing %TORYO,T1: {lines:?}"));
        let resign_idx = lines
            .iter()
            .position(|l| l == "#RESIGN")
            .unwrap_or_else(|| panic!("missing #RESIGN: {lines:?}"));
        assert!(toryo_idx < resign_idx);
        assert_eq!(resign_idx, lines.len() - 1, "result code must be last: {lines:?}");
    }

    /// シナリオ 4: 終局済 DO 接続経路 (moves 全部 + finalized で snapshot を 1 回送る)。
    /// シナリオ 3 と入力は似るが、観戦者が「new connection した時点で既に finished」
    /// だったケース。snapshot は 1 回送って close する経路 (DO 側) なので、本関数の
    /// 戻り値としてはシナリオ 3 と同じ「全 moves + 結果コード」になる。
    #[test]
    fn snapshot_for_finished_do_emits_full_history_with_result_code() {
        let cfg = baseline_config();
        let moves = vec![
            move_row(1, "black", "+7776FU,T3"),
            move_row(2, "white", "-3334FU,T2"),
        ];
        let cl = clocks(594_000, 597_000, Color::Black);
        let finished = FinishedState {
            result_code: "#TIME_UP".to_owned(),
            ended_at_ms: 1_010_000,
        };
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &moves,
            clocks: &cl,
            finalized: Some(&finished),
        });

        // `#TIME_UP` で終端する。
        assert_eq!(lines.last().map(String::as_str), Some("#TIME_UP"));
        // 全 moves が含まれる (順序保証は他テストで確認済みのため、ここでは含有のみ)。
        for m in &moves {
            assert!(
                lines.iter().any(|l| l == &m.line),
                "missing move line {:?}: {lines:?}",
                m.line
            );
        }
    }

    /// `Game_ID:` / `Name+:` / `Name-:` は config 由来で snapshot に乗る。
    #[test]
    fn snapshot_summary_includes_game_id_and_player_names() {
        let cfg = baseline_config();
        let cl = clocks(600_000, 600_000, Color::Black);
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &[],
            clocks: &cl,
            finalized: None,
        });
        assert!(lines.iter().any(|l| l == "Game_ID:room-1-test"));
        assert!(lines.iter().any(|l| l == "Name+:alice"));
        assert!(lines.iter().any(|l| l == "Name-:bob"));
    }
}
