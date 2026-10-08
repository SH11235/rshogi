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
//! 2. これまでの move 行 (1 手あたり 1〜2 行): まず `<token>,T<elapsed_units>`
//!    （broadcast の通常形式と一致。`elapsed_units` は Time_Unit 単位で、`at_ms` 差分から再接続時計補償を
//!    差し引いて再計算する）、
//!    続いてコメントが付いていれば `'<comment>` 行（Floodgate 評価値 PV。ライブ
//!    broadcast の観戦者専用コメント行と同一形式）
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

/// 1 手受理後に観戦者へ送る権威時計の wire 行を組み立てる。
///
/// 指し手の `<token>,T<units>` 行だけでは、観戦 client がTime_Unit 単位へ丸められた消費時間と
/// clock 種別から残時間を再計算する必要がある。サーバーが保持する ms 粒度の本体残時間を
/// 指し手直後に送ることで、client は表示用 countdown の anchor を毎手再同期できる。
///
/// `ply` は直前の指し手と同じ値を入れ、client が現在盤面と結び付ける。snapshot 送信中の
/// pending queue では clock 行を non-move として必ず flush する。これにより snapshot の
/// moves 読み込みと clock 採取が競合しても、最後の毎手 clock が古い snapshot clock を
/// 上書きできる。JSON payload は旧 client が未知の `##[CLOCK]` 行を安全に無視できる追加
/// wire である。
pub(crate) fn build_spectator_clock_update(clocks: &SpectatorClocks, ply: u32) -> String {
    let side_to_move = match clocks.side_to_move {
        Color::Black => "sente",
        Color::White => "gote",
    };
    format!(
        "##[CLOCK] {{\"black_remaining_ms\":{},\"white_remaining_ms\":{},\"side_to_move\":\"{side_to_move}\",\"ply\":{ply}}}",
        clocks.black_remaining_ms, clocks.white_remaining_ms
    )
}

/// core を復元できない終局済み DO でも Game_Summary を返すための時計 fallback。
pub(crate) fn initial_spectator_clocks(config: &PersistedConfig) -> SpectatorClocks {
    let clock = config.clock.build_clock();
    SpectatorClocks {
        black_remaining_ms: clock.remaining_main_ms(Color::Black).max(0) as u64,
        white_remaining_ms: clock.remaining_main_ms(Color::White).max(0) as u64,
        side_to_move: match config.initial_sfen.as_deref() {
            Some(sfen) => side_to_move_from_sfen(sfen).unwrap_or(Color::Black),
            None => Color::Black,
        },
    }
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
        entering_king_rule: input.config.entering_king_rule(),
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

    // 既存の指し手を broadcast と同一 wire 形式に正規化して push する。
    // `MoveRow::line` は client が送ってきた raw 行 (`+7776FU,T3` や Floodgate
    // 形式 `+7776FU,'* 123 +7776FU...`) をそのまま保持しているため、
    // - token 部を取り出し、消費時間は broadcast 同様 `at_ms` 差分から再接続時計
    //   補償を差し引いて再計算した `<token>,T<units>` を出す
    //   (raw 行の `T` 値や `,T` 欠落に依存しない)
    // - コメントが付いていれば直後に `'<comment>` 行を 1 行足す (Floodgate
    //   評価値 PV。ライブ broadcast の観戦者専用コメント行と同じ形式で、観戦
    //   client は `'` 始まり行を無視する互換性がある)
    // export (`export_kifu_to_r2`) と同じ行 parse (`parse_move_row_line`) /
    // elapsed 計算を共有し、wire では Time_Unit、CSA V2 棋譜では秒を使う。
    let first_prev_ms = input.config.play_started_at_ms.unwrap_or(input.config.matched_at_ms);
    let elapsed = move_elapsed_units(input.moves, first_prev_ms, input.config.clock.time_unit_ms());
    for (m, units) in input.moves.iter().zip(elapsed) {
        let (token, comment) = parse_move_row_line(&m.line);
        lines.push(format!("{token},T{units}"));
        if let Some(c) = comment {
            lines.push(format!("'{c}"));
        }
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

/// `MoveRow::line` の raw CSA 行を `(token, comment)` に分解する共有ヘルパ。
///
/// `MoveRow::line` は client が送ってきた行をそのまま保持しており、次の形が
/// あり得る:
/// - `+7776FU` （コメント無し・時間フィールド無し）
/// - `+7776FU,T3` （時間フィールド付き）
/// - `+7776FU,'* 123 +7776FU -3334FU` （Floodgate 形式のコメント付き）
/// - `+7776FU,T3'コメント` （時間 + コメント）
///
/// `token` は最初の `,` より前 (`command.rs::parse_move` と同じ token 抽出)。
/// `comment` は最初の `'` より後 (存在すれば。`'` プレフィックスは含めない)。
/// snapshot (`build_spectator_snapshot`) と export (`export_kifu_to_r2`) で
/// この 1 箇所を共有し、wire 形式の食い違いを防ぐ。
pub(crate) fn parse_move_row_line(line: &str) -> (&str, Option<&str>) {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let token = trimmed.split(',').next().unwrap_or(trimmed);
    let comment = trimmed.split_once('\'').map(|(_, c)| c);
    (token, comment)
}

/// `moves` を先頭から走査し、各手の消費時間を時計の単位で切り捨て、棋譜用の秒へ換算して `at_ms` 差分から
/// 算出する共有ヘルパ。
///
/// `first_prev_ms` は初手の計時起点 (= `play_started_at_ms`、無ければ
/// `matched_at_ms`)。以降は 1 手前の raw `at_ms` を起点にし、各行の差分から
/// `reconnect_credit_ms` を差し引く。これにより次手番の起点は実際の着手時刻の
/// まま、補償対象手の `T` だけを core 課金と同じ実効経過時間へ揃える。負値
/// `at_ms` / credit は 0 に丸める。snapshot と export で同じ補償計算を使い、出力単位だけを変える。
pub(crate) fn move_elapsed_secs(moves: &[MoveRow], first_prev_ms: u64, unit_ms: u64) -> Vec<u32> {
    move_elapsed_units(moves, first_prev_ms, unit_ms)
        .into_iter()
        .map(|units| ((units * unit_ms) / 1000).min(u64::from(u32::MAX)) as u32)
        .collect()
}

// wire は時計の単位、CSA V2 棋譜は秒で表現する。再接続補償の控除を共有する。
fn move_elapsed_units(moves: &[MoveRow], first_prev_ms: u64, unit_ms: u64) -> Vec<u64> {
    let mut prev_ts = first_prev_ms;
    moves
        .iter()
        .map(|m| {
            let at_ms = m.at_ms.max(0) as u64;
            let raw_elapsed_ms = at_ms.saturating_sub(prev_ts);
            prev_ts = at_ms;
            let credit_ms = m.reconnect_credit_ms.max(0) as u64;
            let elapsed_ms = raw_elapsed_ms.saturating_sub(credit_ms);
            elapsed_ms / unit_ms
        })
        .collect()
}

/// ミリ秒時計の経過時間を CSA コメントとして保存する。
/// V2 本文の T は秒を維持し、終局後の観戦履歴だけコメントから精度を復元する。
pub(crate) fn with_millisecond_timings(csa: &str, moves: &[MoveRow], first_prev_ms: u64) -> String {
    use std::fmt::Write as _;
    let elapsed = move_elapsed_units(moves, first_prev_ms, 1);
    let mut timings = elapsed.into_iter();
    let mut out = String::with_capacity(csa.len());
    for line in csa.lines() {
        let _ = writeln!(out, "{line}");
        if line.starts_with(['+', '-'])
            && line.len() >= 7
            && let Some(ms) = timings.next()
        {
            let _ = writeln!(out, "'RSHOGI_TIME_MS:{ms}");
        }
    }
    out
}

/// export 済み CSA V2 本文から snapshot 用の指し手列を復元する。
///
/// `KifuRecord::build_v2` が出す通常手 (`+7776FU,T3`) と直後のコメント行だけを
/// `MoveRow` 互換に戻す。終局後は DO の `moves` テーブルを cleanup するため、
/// late-joiner snapshot は R2 / export pending の CSA 本文を正とする。
/// ミリ秒時計は `RSHOGI_TIME_MS` コメントから精度を復元し、旧棋譜は秒へフォールバックする。
pub(crate) fn move_rows_from_exported_csa(csa_text: &str, first_prev_ms: u64) -> Vec<MoveRow> {
    let mut rows: Vec<MoveRow> = Vec::new();
    let mut cumulative_ms = 0_u64;
    let mut has_ms_timing = false;

    for raw in csa_text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.starts_with(['+', '-']) && line.len() >= 7 {
            let token = &line[..7];
            let elapsed_sec = line[7..]
                .strip_prefix(",T")
                .and_then(|rest| {
                    let digits_len = rest.bytes().take_while(u8::is_ascii_digit).count();
                    (digits_len > 0).then(|| rest[..digits_len].parse::<u32>().ok()).flatten()
                })
                .unwrap_or(0);
            cumulative_ms = cumulative_ms.saturating_add(u64::from(elapsed_sec) * 1000);
            let at_ms = first_prev_ms.saturating_add(cumulative_ms).min(i64::MAX as u64) as i64;
            has_ms_timing = false;
            rows.push(MoveRow {
                ply: i64::try_from(rows.len() + 1).unwrap_or(i64::MAX),
                color: if token.starts_with('+') {
                    "black"
                } else {
                    "white"
                }
                .to_owned(),
                line: line.to_owned(),
                at_ms,
                reconnect_credit_ms: 0,
            });
        } else if let Some(ms) = line.strip_prefix("'RSHOGI_TIME_MS:") {
            if !has_ms_timing && let Ok(ms) = ms.parse::<u64>() {
                // 直前の秒丸め時刻を、保存済みの実効経過 ms で置換する。
                let prev_ms = if rows.len() >= 2 {
                    rows[rows.len() - 2].at_ms.max(0) as u64
                } else {
                    first_prev_ms
                };
                if let Some(last) = rows.last_mut() {
                    has_ms_timing = true;
                    last.at_ms = prev_ms.saturating_add(ms).min(i64::MAX as u64) as i64;
                    cumulative_ms = (last.at_ms as u64).saturating_sub(first_prev_ms);
                }
            }
        } else if let Some(comment) = line.strip_prefix('\'')
            && let Some(last) = rows.last_mut()
            && !last.line.contains('\'')
        {
            last.line.push('\'');
            last.line.push_str(comment);
        }
    }

    rows
}

/// broadcast entry が「盤面を進めた指し手行」かを判定する共有ヘルパ。
///
/// 指し手 broadcast は `+`/`-` で始まり `ply.is_some()`。終局理由行 (`%TORYO`
/// 等) や勝敗コード行 (`#RESIGN` 等)、観戦者専用コメント行 (`'...`) はいずれも
/// これに該当しない。workers の `finalize`（`GameEnded` 経路）が「盤面を進めた
/// 終局手を moves テーブルへ永続化すべきか」を判定するのに使う
/// (https://github.com/SH11235/rshogi/issues/853 系: 終局手の棋譜欠落防止)。
pub(crate) fn is_move_broadcast(entry: &rshogi_csa_server::BroadcastEntry) -> bool {
    entry.ply.is_some() && entry.line.as_str().starts_with(['+', '-'])
}

/// 毎手 clock を挿入する broadcast index と ply を返す。
///
/// Floodgate コメントは「直前の指し手に属する」wire 契約なので、move と同じ ply の
/// 観戦者専用 `'` 行をすべて通過した位置を返す。終局手では、その後に続く結果行より
/// 前へ clock を挿入する。1 回の HandleResult に盤面を進める move は高々 1 件。
pub(crate) fn spectator_clock_insert_after(
    entries: &[rshogi_csa_server::BroadcastEntry],
) -> Option<(usize, u32)> {
    let move_index = entries.iter().position(is_move_broadcast)?;
    let ply = entries[move_index].ply?;
    let mut insert_after = move_index;
    while let Some(entry) = entries.get(insert_after + 1) {
        let is_attached_comment = entry.ply == Some(ply)
            && matches!(entry.target, rshogi_csa_server::BroadcastTarget::Spectators)
            && entry.line.as_str().starts_with('\'');
        if !is_attached_comment {
            break;
        }
        insert_after += 1;
    }
    Some((insert_after, ply))
}

#[cfg(test)]
mod tests {
    use rshogi_csa_server::ClockSpec;

    use super::*;

    fn baseline_config() -> PersistedConfig {
        PersistedConfig {
            game_id: "room-1-test".to_owned(),
            black_handle: "alice".to_owned(),
            black_player_id: crate::player_identity::legacy_player_id("alice"),
            white_handle: "bob".to_owned(),
            white_player_id: crate::player_identity::legacy_player_id("bob"),
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
            entering_king_rule: None,
        }
    }

    /// `at_ms` を明示して MoveRow を作る (経過秒の正規化テスト用)。
    fn move_row_at(ply: i64, color: &str, line: &str, at_ms: i64) -> MoveRow {
        MoveRow {
            ply,
            color: color.to_owned(),
            line: line.to_owned(),
            at_ms,
            reconnect_credit_ms: 0,
        }
    }

    fn clocks(black: u64, white: u64, side: Color) -> SpectatorClocks {
        SpectatorClocks {
            black_remaining_ms: black,
            white_remaining_ms: white,
            side_to_move: side,
        }
    }

    #[test]
    fn snapshot_uses_clock_units_while_export_keeps_seconds() {
        for (clock, elapsed_ms, wire_t, seconds) in [
            (
                ClockSpec::CountdownMsec {
                    total_time_ms: 10000,
                    byoyomi_ms: 0,
                },
                1250,
                1250,
                1,
            ),
            (
                ClockSpec::StopWatch {
                    total_time_min: 3,
                    byoyomi_min: 0,
                },
                125000,
                2,
                120,
            ),
        ] {
            let mut cfg = baseline_config();
            cfg.clock = clock;
            // raw 経過に再接続補償 250ms が含まれていても、ライブと同じ課金にする。
            let mut row = move_row_at(1, "black", "+7776FU,T99", 1_000_000 + elapsed_ms + 250);
            row.reconnect_credit_ms = 250;
            let rows = [row];
            let cl = clocks(10000, 10000, Color::White);
            let lines = build_spectator_snapshot(SpectatorSnapshotInput {
                config: &cfg,
                moves: &rows,
                clocks: &cl,
                finalized: None,
            });
            assert!(lines.contains(&format!("+7776FU,T{wire_t}")));
            assert_eq!(move_elapsed_secs(&rows, 1_000_000, cfg.clock.time_unit_ms()), [seconds]);
        }
    }

    #[test]
    fn finished_snapshot_retains_milliseconds_after_csa_roundtrip() {
        let mut cfg = baseline_config();
        cfg.clock = ClockSpec::CountdownMsec {
            total_time_ms: 10000,
            byoyomi_ms: 100,
        };
        let mut first = move_row_at(1, "black", "+7776FU", 1_000_500);
        first.reconnect_credit_ms = 250;
        let rows = [first, move_row_at(2, "white", "-3334FU", 1_001_750)];
        let csa = with_millisecond_timings(
            "V2.2\nPI\n+\n+7776FU,T0\n'** 50 -3334FU\n-3334FU,T1\n%TORYO\n",
            &rows,
            1_000_000,
        );
        assert!(csa.contains("+7776FU,T0\n'RSHOGI_TIME_MS:250"));
        assert!(csa.contains("-3334FU,T1\n'RSHOGI_TIME_MS:1250"));
        let restored = move_rows_from_exported_csa(&csa, 1_000_000);
        assert_eq!(move_elapsed_units(&restored, 1_000_000, 1), [250, 1250]);
        assert_eq!(parse_move_row_line(&restored[0].line).1, Some("** 50 -3334FU"));
        let cl = clocks(9750, 8750, Color::Black);
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &restored,
            clocks: &cl,
            finalized: None,
        });
        assert!(lines.contains(&"+7776FU,T250".into()));
        assert!(lines.contains(&"-3334FU,T1250".into()));
        assert!(!lines.iter().any(|line| line.contains("RSHOGI_TIME_MS")));
        // コメントのない既存棋譜も従来どおり秒として読める。
        let old = move_rows_from_exported_csa("+7776FU,T0\n-3334FU,T1\n", 1_000_000);
        assert_eq!(move_elapsed_units(&old, 1_000_000, 1), [0, 1000]);
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

    /// シナリオ 2: 数手後 (= moves 3 件、進行中)。raw 行は token のみでも、
    /// 出力は `at_ms` 差分から計算した `<token>,T<units>` に正規化される。
    #[test]
    fn snapshot_after_three_moves_appends_move_lines_in_order() {
        let cfg = baseline_config();
        // raw 行は bare token (T フィールド無し)。play_started_at_ms=1_000_000
        // を起点に、各 at_ms 差分から T が算出される。
        let moves = vec![
            move_row_at(1, "black", "+7776FU", 1_003_000), // 3s
            move_row_at(2, "white", "-3334FU", 1_005_000), // 2s
            move_row_at(3, "black", "+8833UM", 1_009_000), // 4s
        ];
        let cl = clocks(597_000, 598_000, Color::White);
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &moves,
            clocks: &cl,
            finalized: None,
        });

        // 正規化後の move 行が順序通り含まれる。
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

    /// シナリオ 3: 経過秒の正規化 + Floodgate コメント行の展開。raw 行の `T` 値は
    /// 無視して `at_ms` 差分から再計算し、コメントは token 行の直後に `'` 始まりで
    /// 1 行足す (ライブ broadcast の観戦者専用コメント行と同一 wire 形式)。
    #[test]
    fn snapshot_normalizes_time_and_emits_comment_lines() {
        let cfg = baseline_config();
        let moves = vec![
            // raw の `,T99` は無視され、at_ms 差分の 3s に正規化される。
            move_row_at(1, "black", "+7776FU,T99", 1_003_000),
            // Floodgate 形式: token 行 + `'` コメント行の 2 行に展開される。
            move_row_at(2, "white", "-3334FU,'* -50 -3334FU +2726FU", 1_006_000),
        ];
        let cl = clocks(597_000, 597_000, Color::Black);
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &moves,
            clocks: &cl,
            finalized: None,
        });

        // raw の T99 は出ず、at_ms 差分の T3 に正規化される。
        assert!(
            lines.iter().any(|l| l == "+7776FU,T3"),
            "missing normalized +7776FU,T3: {lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("T99")), "raw T99 must not leak: {lines:?}");
        // コメントは token 行の直後に `'` 始まりで 1 行。
        let mv_idx = lines
            .iter()
            .position(|l| l == "-3334FU,T3")
            .unwrap_or_else(|| panic!("missing -3334FU,T3: {lines:?}"));
        let cmt_idx = lines
            .iter()
            .position(|l| l == "'* -50 -3334FU +2726FU")
            .unwrap_or_else(|| panic!("missing comment line: {lines:?}"));
        assert_eq!(cmt_idx, mv_idx + 1, "comment must follow its move line: {lines:?}");
    }

    /// シナリオ 4: 終局済 DO 接続経路 (moves 全部 + finalized で snapshot を 1 回送る)。
    /// 観戦者が「new connection した時点で既に finished」だったケース。snapshot は
    /// 1 回送って close する経路 (DO 側) で、戻り値は「全 moves (正規化済) + 結果
    /// コード」になる。
    #[test]
    fn snapshot_for_finished_do_emits_full_history_with_result_code() {
        let cfg = baseline_config();
        let moves = vec![
            move_row_at(1, "black", "+7776FU,T3", 1_002_000), // 2s
            move_row_at(2, "white", "-3334FU,T2", 1_005_000), // 3s
        ];
        let cl = clocks(594_000, 597_000, Color::Black);
        let finished = FinishedState {
            result_code: "#TIME_UP".to_owned(),
            ended_at_ms: 1_010_000,
            exported_at_ms: None,
        };
        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: &cfg,
            moves: &moves,
            clocks: &cl,
            finalized: Some(&finished),
        });

        // `#TIME_UP` で終端する。
        assert_eq!(lines.last().map(String::as_str), Some("#TIME_UP"));
        // 正規化後の token 行 (raw の T 値ではなく at_ms 差分の T) が全て含まれる。
        assert!(lines.iter().any(|l| l == "+7776FU,T2"), "missing +7776FU,T2: {lines:?}");
        assert!(lines.iter().any(|l| l == "-3334FU,T3"), "missing -3334FU,T3: {lines:?}");
    }

    #[test]
    fn initial_spectator_clocks_uses_config_clock_and_sfen_turn() {
        let mut cfg = baseline_config();
        cfg.clock = ClockSpec::CountdownMsec {
            total_time_ms: 10_000,
            byoyomi_ms: 100,
        };
        cfg.initial_sfen =
            Some("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1".to_owned());

        let cl = initial_spectator_clocks(&cfg);

        assert_eq!(cl.black_remaining_ms, 10_000);
        assert_eq!(cl.white_remaining_ms, 10_000);
        assert_eq!(cl.side_to_move, Color::White);
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

    #[test]
    fn parse_move_row_line_extracts_token_and_optional_comment() {
        // bare token: comment 無し。
        assert_eq!(parse_move_row_line("+7776FU"), ("+7776FU", None));
        // T フィールドのみ: comment 無し。
        assert_eq!(parse_move_row_line("+7776FU,T3"), ("+7776FU", None));
        // Floodgate 形式 (`,'`): comment はプレフィックス `'` を除いた本体。
        assert_eq!(
            parse_move_row_line("+7776FU,'* 123 +7776FU -3334FU"),
            ("+7776FU", Some("* 123 +7776FU -3334FU"))
        );
        // T + comment: token は最初の `,` まで、comment は最初の `'` 以降。
        assert_eq!(parse_move_row_line("+7776FU,T3'note"), ("+7776FU", Some("note")));
        // 末尾改行は除去する。
        assert_eq!(parse_move_row_line("+7776FU,T3\r\n"), ("+7776FU", None));
    }

    #[test]
    fn move_rows_from_exported_csa_restores_moves_comments_and_elapsed() {
        let csa = "\
V2.2
N+alice
N-bob
$GAME_ID:g1
PI
+
+7776FU,T3
-3334FU,T4
'eval=12 pv 3c3d
%TORYO
";
        let rows = move_rows_from_exported_csa(csa, 1_000);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ply, 1);
        assert_eq!(rows[0].color, "black");
        assert_eq!(rows[0].line, "+7776FU,T3");
        assert_eq!(rows[0].at_ms, 4_000);
        assert_eq!(rows[1].ply, 2);
        assert_eq!(rows[1].color, "white");
        assert_eq!(rows[1].line, "-3334FU,T4'eval=12 pv 3c3d");
        assert_eq!(rows[1].at_ms, 8_000);
    }

    #[test]
    fn move_elapsed_secs_computes_from_at_ms_deltas() {
        let moves = vec![
            move_row_at(1, "black", "+7776FU", 1_003_000), // prev 1_000_000 → 3s
            move_row_at(2, "white", "-3334FU", 1_005_500), // prev 1_003_000 → 2s (切り捨て)
            move_row_at(3, "black", "+8833UM", 1_005_000), // prev 1_005_500 → 0s (逆行は 0)
        ];
        assert_eq!(move_elapsed_secs(&moves, 1_000_000, 1000), vec![3, 2, 0]);
        // 空入力は空 Vec。
        assert_eq!(move_elapsed_secs(&[], 1_000_000, 1000), Vec::<u32>::new());
    }

    #[test]
    fn move_elapsed_secs_subtracts_credit_without_shifting_the_next_turn_origin() {
        let mut moves = vec![
            move_row_at(1, "black", "+7776FU", 1_005_000),
            move_row_at(2, "white", "-3334FU", 1_007_000),
            move_row_at(3, "black", "+2726FU", 1_010_000),
        ];
        moves[0].reconnect_credit_ms = 3_000;
        moves[2].reconnect_credit_ms = 1_000;

        // 1 手目は raw 5s - credit 3s = T2。2 手目の起点は補償後の仮想時刻でなく
        // 1 手目の raw at_ms のままなので T2、3 手目は raw 3s - credit 1s = T2。
        assert_eq!(move_elapsed_secs(&moves, 1_000_000, 1000), vec![2, 2, 2]);
    }

    #[test]
    fn live_broadcast_t_matches_export_elapsed_secs() {
        // #857 test (c): ライブ配信 T (core `apply_move` の `<token>,T<units>` broadcast) と、
        //   終局 export / 観戦 snapshot の T (`move_elapsed_secs` が at_ms 差分から再計算)
        //   が同一手で一致すること。通信マージンを課金から差し引かなくなったため、両者とも
        //   「(到着 at_ms − 直前手 at_ms) を秒切り捨て」した同じ値になる (旧実装ではライブ側が
        //   margin を引いて 1 秒/手ずれていた)。
        use rshogi_csa_server::{CsaLine, FischerClock, GameRoom, GameRoomConfig, HandleOutcome};

        let play_started_ms: u64 = 1_000_000;
        let config = GameRoomConfig {
            game_id: GameId::new("20140101120000"),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            max_moves: 256,
            // margin を課金へ持ち込まないことを確認するため 0 でなく 1000ms を設定する。
            time_margin_ms: 1_000,
            entering_king_rule: crate::config::DEFAULT_ENTERING_KING_RULE,
            initial_sfen: None,
        };
        let clock = Box::new(FischerClock::new(60, 5));
        let mut room = GameRoom::new(config, clock).expect("valid test config");
        // AGREE を play_started_ms で成立させる (初手の計時起点 = play_started_ms)。
        room.handle_line(Color::Black, &CsaLine::new("AGREE"), play_started_ms).unwrap();
        room.handle_line(Color::White, &CsaLine::new("AGREE"), play_started_ms).unwrap();

        // (color, token, at_ms) の着手列。秒境界の端数 (margin を跨ぐ差分) を含める。
        let seq: [(Color, &str, u64); 4] = [
            (Color::Black, "+7776FU", play_started_ms + 3_500), // 3500ms → T3
            (Color::White, "-3334FU", play_started_ms + 8_000), // 4500ms → T4
            (Color::Black, "+2726FU", play_started_ms + 10_000), // 2000ms → T2
            (Color::White, "-8384FU", play_started_ms + 15_200), // 5200ms → T5
        ];

        let mut live_lines: Vec<String> = Vec::new();
        let mut move_rows: Vec<MoveRow> = Vec::new();
        for (i, (color, token, at_ms)) in seq.iter().enumerate() {
            let r = room.handle_line(*color, &CsaLine::new(*token), *at_ms).unwrap();
            assert!(matches!(r.outcome, HandleOutcome::MoveAccepted { .. }));
            // broadcasts[0] が `<token>,T<units>` (ライブ配信 T)。
            live_lines.push(r.broadcasts[0].line.as_str().to_owned());
            let color_str = if matches!(color, Color::Black) {
                "+"
            } else {
                "-"
            };
            move_rows.push(move_row_at(i as i64 + 1, color_str, token, *at_ms as i64));
        }

        // export / snapshot 経路の T を at_ms 差分から再計算し、ライブ配信 T と一致を確認。
        let export_secs = move_elapsed_secs(&move_rows, play_started_ms, 1000);
        assert_eq!(export_secs.len(), live_lines.len());
        for (i, (_, token, _)) in seq.iter().enumerate() {
            assert_eq!(live_lines[i], format!("{token},T{}", export_secs[i]));
        }
        // 具体値も pin する。
        assert_eq!(export_secs, vec![3, 4, 2, 5]);
    }

    #[test]
    fn is_move_broadcast_only_matches_board_advancing_moves() {
        use rshogi_csa_server::types::CsaLine;
        use rshogi_csa_server::{BroadcastEntry, BroadcastTarget};

        let mv = BroadcastEntry {
            target: BroadcastTarget::All,
            line: CsaLine::new("+7776FU,T3"),
            ply: Some(1),
        };
        assert!(is_move_broadcast(&mv));

        // 観戦者専用コメント行 (`'...`) は move ではない。
        let comment = BroadcastEntry {
            target: BroadcastTarget::Spectators,
            line: CsaLine::new("'* 123 +7776FU"),
            ply: Some(1),
        };
        assert!(!is_move_broadcast(&comment));

        // 終局理由行 / 勝敗コード行 (ply=None) は move ではない。
        let toryo = BroadcastEntry {
            target: BroadcastTarget::All,
            line: CsaLine::new("#RESIGN"),
            ply: None,
        };
        assert!(!is_move_broadcast(&toryo));

        // '+'/'-' 始まりでも ply=None なら move ではない (防御的)。
        let no_ply = BroadcastEntry {
            target: BroadcastTarget::All,
            line: CsaLine::new("+7776FU,T3"),
            ply: None,
        };
        assert!(!is_move_broadcast(&no_ply));
    }

    #[test]
    fn spectator_clock_update_is_structured_and_ply_bound() {
        let clocks = SpectatorClocks {
            black_remaining_ms: 445_123,
            white_remaining_ms: 405_987,
            side_to_move: Color::White,
        };

        assert_eq!(
            build_spectator_clock_update(&clocks, 48),
            "##[CLOCK] {\"black_remaining_ms\":445123,\"white_remaining_ms\":405987,\"side_to_move\":\"gote\",\"ply\":48}"
        );
    }

    #[test]
    fn spectator_clock_is_inserted_after_comment_and_before_terminal_lines() {
        use rshogi_csa_server::{BroadcastEntry, BroadcastTarget, CsaLine};

        let entries = vec![
            BroadcastEntry {
                target: BroadcastTarget::All,
                line: CsaLine::new("+7776FU,T3"),
                ply: Some(1),
            },
            BroadcastEntry {
                target: BroadcastTarget::Spectators,
                line: CsaLine::new("'* 123 -3334FU"),
                ply: Some(1),
            },
            BroadcastEntry {
                target: BroadcastTarget::Spectators,
                line: CsaLine::new("#MAX_MOVES"),
                ply: None,
            },
        ];

        assert_eq!(spectator_clock_insert_after(&entries), Some((1, 1)));
        assert_eq!(spectator_clock_insert_after(&entries[..1]), Some((0, 1)));
    }
}
