//! 棋譜記録・保存

use std::fmt::Write as _;

use anyhow::Result;
use chrono::Local;

use rshogi_csa::{Color, Position, csa_move_to_usi};

use crate::config::RecordConfig;
use crate::csa_writer::{
    CsaGameEnd, CsaGameHeader, CsaTimeLimit, EndReason, MoveEval, WdoorCsaWriter, jst_naive,
};
use crate::engine::SearchInfo;
use crate::protocol::{GameResult, GameSummary, TimeConfig};

/// 対局中に蓄積する棋譜データ
#[derive(Clone, Debug)]
pub struct GameRecord {
    pub game_id: String,
    pub sente_name: String,
    pub gote_name: String,
    pub black_time: TimeConfig,
    pub white_time: TimeConfig,
    /// 対局開始時の局面
    pub initial_position: Position,
    pub moves: Vec<RecordedMove>,
    /// 終局理由。[`EndReason::as_str`] の値か、サーバーが理由行を送らなかったときの
    /// `win` / `lose`。未終局・切断は空文字。JSONL の `result.reason` にも同じ値が入る。
    pub result: String,
    /// 勝者。引き分け・中断・未終局は `None`。CSA の `'summary:` 行に使う。
    pub winner: Option<Color>,
    pub start_time: chrono::DateTime<Local>,
    /// 終局を記録した時刻。CSA の `'$END_TIME:` 行に使う。
    pub end_time: Option<chrono::DateTime<Local>>,
    /// 送ったがサーバーに受理されなかった自分の最後の手（CSA 形式）。反則負けや
    /// 時間切れでは手のエコーが返らず、その手は対局の指し手に含まれない。`moves` には
    /// 入れず、反則負けのとき CSA に `'ILLEGAL_MOVE(<手>)` コメントとして残す。
    pub rejected_move: Option<String>,
    /// 自エンジンの手番。JSONL 出力モードで `outcome` / `winner` の正規化に使う。
    pub my_color: Color,
    /// JSONL 出力モード用に蓄積する手単位の追加情報。CSA / SFEN 棋譜出力には影響しない。
    /// 各要素は `moves[i]` に対応する。投了 / 勝ち宣言など `apply_csa_move` を経由しない
    /// 手は含まれず、ply ベースで一致する。
    pub jsonl_moves: Vec<JsonlMoveExtra>,
}

/// 棋譜が対局全体を表すか、再接続後だけの断片かを示す。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RecordStatus {
    /// 対局開始からの通し棋譜。
    #[default]
    Complete,
    /// 再接続時に、切断前の保持局面とサーバーの resume 局面が一致しなかったため
    /// resume 局面から記録し直した断片。
    ReconnectFragment { reason: String },
}

/// 切断前の棋譜を resume セッションへ引き継げるかの判定結果。
#[derive(Clone, Debug)]
pub(crate) enum ResumeRecordDecision {
    /// 局面が一致したため、切断前の棋譜・開始時刻・JSONL 情報を引き継ぐ。
    Continued {
        record: Box<GameRecord>,
        position: Box<Position>,
        usi_moves: Vec<String>,
    },
    /// 局面または対局 identity が一致しないため、resume 局面から断片記録を始める。
    Fragment {
        record: Box<GameRecord>,
        retained_record: Box<GameRecord>,
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct RecordedMove {
    pub csa_move: String,
    pub time_sec: u32,
    pub eval_cp: Option<i32>,
    pub eval_mate: Option<i32>,
    pub depth: Option<u32>,
    pub pv: Vec<String>,
    /// この手を指した側の手番（評価値の先手視点正規化に使用）
    pub side_to_move: Color,
}

/// JSONL 出力モードで `move` 行に書く追加情報。
///
/// `analyze_selfplay` が読み取るスキーマ（`tools/src/bin/tournament.rs` 互換）と
/// 揃えるための転写領域。生成は `session.rs` の対局ループで行う。
#[derive(Clone, Debug)]
pub struct JsonlMoveExtra {
    /// この手を指す前の SFEN（`position` コマンドで送ったのと同じ手前局面）
    pub sfen_before: String,
    /// USI 形式の指し手
    pub move_usi: String,
    /// 公開 API の互換性のため保持する呼出側のラベル。JSONL 出力には使用しない。
    /// 出力の識別子は `GameRecord` の名前と手番から生成する。
    pub engine_label: String,
    /// この手の探索に費やした実時間 (ms)
    pub elapsed_ms: u64,
    /// `go` で指示した考慮上限 (ms)。byoyomi+残時間ベースで session.rs が計算した値。
    pub think_limit_ms: u64,
    /// USI `info` から最後に観測した seldepth
    pub seldepth: Option<u32>,
    /// USI `info` から最後に観測した nodes
    pub nodes: Option<u64>,
    /// USI `info` から最後に観測した time
    pub time_ms: Option<u64>,
    /// USI `info` から最後に観測した nps
    pub nps: Option<u64>,
}

impl RecordedMove {
    /// 評価値を先手視点に正規化して返す。
    /// USI の score cp/mate は手番側視点なので、後手番なら符号を反転する。
    pub fn effective_score(&self) -> Option<i32> {
        let raw = if let Some(cp) = self.eval_cp {
            Some(cp)
        } else {
            self.eval_mate.map(|m| if m > 0 { 100000 } else { -100000 })
        };
        raw.and_then(|v| match self.side_to_move {
            Color::Black => Some(v),
            Color::White => v.checked_neg(),
        })
    }
}

impl GameRecord {
    pub fn new(summary: &GameSummary) -> Self {
        Self {
            game_id: summary.game_id.clone(),
            sente_name: summary.sente_name.clone(),
            gote_name: summary.gote_name.clone(),
            black_time: summary.black_time.clone(),
            white_time: summary.white_time.clone(),
            initial_position: summary.position.clone(),
            moves: Vec::new(),
            result: String::new(),
            winner: None,
            start_time: Local::now(),
            end_time: None,
            rejected_move: None,
            my_color: summary.my_color,
            jsonl_moves: Vec::new(),
        }
    }

    /// 再接続前に保持していた棋譜を、サーバーから再送された現在局面と照合する。
    ///
    /// `Game_Summary` の Position block には手数番号が無いため、比較時は `ply` を
    /// 除外し、盤面・持ち駒・手番を比較する。一致時は保持棋譜を初期局面から replay
    /// して得た `position` / `usi_moves` も返し、resume 後の USI `position` コマンドを
    /// 対局開始局面から通しで組み立てられるようにする。
    pub(crate) fn continue_for_resume(
        self,
        summary: &GameSummary,
        reconnect_turn: Option<Color>,
    ) -> ResumeRecordDecision {
        let mismatch = |reason: String| {
            let mut record = GameRecord::new(summary);
            // fallback でも対局開始時刻は維持する。ファイル名と $START_TIME が
            // 再接続時刻へ巻き戻ると、元の欠損バグと同じ誤認を招くため。
            record.start_time = self.start_time;
            ResumeRecordDecision::Fragment {
                record: Box::new(record),
                // live JSONL が有効だった場合、切断前の通常名ファイルを削除するため
                // 元 record も返す（不一致時だけの clone）。
                retained_record: Box::new(self.clone()),
                reason,
            }
        };

        if self.game_id != summary.game_id {
            return mismatch(format!(
                "game_id 不一致: retained={} resume={}",
                self.game_id, summary.game_id
            ));
        }
        if self.sente_name != summary.sente_name || self.gote_name != summary.gote_name {
            return mismatch(format!(
                "対局者不一致: retained={} vs {} resume={} vs {}",
                self.sente_name, self.gote_name, summary.sente_name, summary.gote_name
            ));
        }
        if self.my_color != summary.my_color {
            return mismatch(format!(
                "自手番不一致: retained={:?} resume={:?}",
                self.my_color, summary.my_color
            ));
        }

        let mut retained_position = self.initial_position.clone();
        let mut usi_moves = Vec::with_capacity(self.moves.len());
        for (index, recorded) in self.moves.iter().enumerate() {
            let usi = match csa_move_to_usi(&recorded.csa_move, &retained_position) {
                Ok(usi) => usi,
                Err(err) => {
                    return mismatch(format!(
                        "保持棋譜の {} 手目を USI 変換できません: {} ({err:#})",
                        index + 1,
                        recorded.csa_move
                    ));
                }
            };
            if let Err(err) = retained_position.apply_csa_move(&recorded.csa_move) {
                return mismatch(format!(
                    "保持棋譜の {} 手目を適用できません: {} ({err:#})",
                    index + 1,
                    recorded.csa_move
                ));
            }
            usi_moves.push(usi);
        }

        // Position block 内に初期手順を持つ CSA サーバーにも対応する。Workers の
        // reconnect summary は現在局面そのものを Position block に入れるため通常は空。
        let mut resume_position = summary.position.clone();
        for cm in &summary.initial_moves {
            if let Err(err) = resume_position.apply_csa_move(&cm.mv) {
                return mismatch(format!(
                    "resume Game_Summary の指し手を適用できません: {} ({err:#})",
                    cm.mv
                ));
            }
        }

        match reconnect_turn {
            Some(turn) if turn == resume_position.side_to_move => {}
            Some(turn) => {
                return mismatch(format!(
                    "Reconnect_State の手番不一致: summary={:?} reconnect_state={turn:?}",
                    resume_position.side_to_move
                ));
            }
            None => {
                return mismatch("Reconnect_State に Current_Turn がありません".to_owned());
            }
        }

        if retained_position.to_csa_board() != resume_position.to_csa_board() {
            return mismatch(format!(
                "局面不一致: retained={} resume={}",
                retained_position.to_sfen(),
                resume_position.to_sfen()
            ));
        }

        ResumeRecordDecision::Continued {
            record: Box::new(self),
            position: Box::new(retained_position),
            usi_moves,
        }
    }

    /// JSONL 出力モード向けの追加情報を 1 手分蓄積する。
    /// CSA 棋譜・SFEN 出力にはこのバッファは使われない。
    pub fn add_jsonl_move(&mut self, extra: JsonlMoveExtra) {
        self.jsonl_moves.push(extra);
    }

    pub fn add_move(
        &mut self,
        csa_move: &str,
        time_sec: u32,
        info: Option<&SearchInfo>,
        side_to_move: Color,
    ) {
        let (eval_cp, eval_mate, depth, pv) = match info {
            Some(i) => (i.score_cp, i.score_mate, i.depth, i.pv.clone()),
            None => (None, None, None, Vec::new()),
        };
        self.moves.push(RecordedMove {
            csa_move: csa_move.to_string(),
            time_sec,
            eval_cp,
            eval_mate,
            depth,
            pv,
            side_to_move,
        });
    }

    /// 最後の手の消費時間を更新する（サーバーエコーで確定した値）
    pub fn update_last_time(&mut self, time_sec: u32) {
        if let Some(last) = self.moves.last_mut() {
            last.time_sec = time_sec;
        }
    }

    /// 終局理由だけを記録する。サーバーの最終結果を待つ前（自分の `%TORYO` /
    /// `%KACHI` 送信直後）に使い、結果が届いたら [`GameRecord::set_game_end`] で確定する。
    pub fn set_result(&mut self, result: &str) {
        self.result = result.to_string();
    }

    /// サーバーのエコーが返らないまま終局した自分の最後の手を、指し手列から外す。
    ///
    /// CSA サーバーは受理した手を消費時間つきでエコーしてから終局を通知する。エコーなしで
    /// 終局した手（反則手、時間切れ後に届いた手など）は対局の指し手ではないので、棋譜に
    /// 残すとサーバーの記録と食い違い、合法手として再生できなくなる。外した手は
    /// `rejected_move` に残す。
    pub fn reject_unconfirmed_last_move(&mut self) {
        let Some(last) = self.moves.pop() else {
            return;
        };
        // JSONL 用の追加情報は `moves` と同時に積まれる。対応が崩れないよう一緒に外す。
        if self.jsonl_moves.len() > self.moves.len() {
            self.jsonl_moves.truncate(self.moves.len());
        }
        self.rejected_move = Some(last.csa_move);
    }

    /// 終局理由とサーバーの最終結果を記録する。勝者と終局時刻もここで確定する。
    pub fn set_game_end(&mut self, reason: &str, result: &GameResult) {
        self.result = reason.to_string();
        self.winner = match result {
            GameResult::Win => Some(self.my_color),
            GameResult::Lose => Some(opposite(self.my_color)),
            GameResult::Draw | GameResult::Interrupted | GameResult::Censored => None,
        };
        self.end_time = Some(Local::now());
    }

    /// CSA形式の棋譜テキストを生成する
    pub fn to_csa(&self) -> String {
        self.to_csa_with_status(&RecordStatus::Complete)
    }

    /// record の種別を明示して CSA 形式の棋譜テキストを生成する。
    ///
    /// 書式は wdoor の公開棋譜と同じ（指し手 → `T<秒>` → `'** <先手視点評価値> <読み筋>`、
    /// 末尾に `'summary:` と `'$END_TIME:`）。時刻は JST。詳細は [`crate::csa_writer`] を参照。
    pub fn to_csa_with_status(&self, status: &RecordStatus) -> String {
        let leading_comment = match status {
            RecordStatus::Complete => None,
            RecordStatus::ReconnectFragment { reason } => {
                Some(format!("RECONNECT_FRAGMENT: {reason}"))
            }
        };
        // 先手の時間設定を $TIME_LIMIT に出力（CSA標準）
        let header = CsaGameHeader {
            sente_name: &self.sente_name,
            gote_name: &self.gote_name,
            event: &self.game_id,
            // CSA の時刻にはタイムゾーン表記がなく、棋譜サーバーは JST として読む。
            start_time: jst_naive(&self.start_time),
            time_limit: Some(CsaTimeLimit {
                total_sec: ms_to_sec(self.black_time.total_time_ms),
                byoyomi_sec: ms_to_sec(self.black_time.byoyomi_ms),
                increment_sec: ms_to_sec(self.black_time.increment_ms),
            }),
            leading_comment: leading_comment.as_deref(),
        };
        let mut writer = WdoorCsaWriter::new(&header, &self.initial_position);
        for m in &self.moves {
            let eval = m.effective_score().map(|score_black| MoveEval {
                score_black,
                pv_usi: &m.pv,
            });
            // 適用できない手が混じっても棋譜の行は残す。以降の読み筋は変換できず
            // 評価値だけになる。
            if let Err(err) = writer.push_move(&m.csa_move, u64::from(m.time_sec), eval) {
                log::warn!("[REC] 棋譜の指し手を局面に適用できません: {} ({err:#})", m.csa_move);
            }
        }
        let end = EndReason::from_record_result(&self.result).map(|reason| CsaGameEnd {
            reason,
            winner: self.winner,
            end_time: self.end_time.as_ref().map(jst_naive),
            rejected_move: self.rejected_move.clone(),
        });
        writer.finish(end.as_ref())
    }

    /// SFEN局面列を生成する（学習データ用）。
    /// 形式: `<SFEN>\t<USI指し手>\t<先手視点評価値>`
    pub fn to_sfen_lines(&self) -> Result<String> {
        use rshogi_csa::csa_move_to_usi;

        let mut pos = self.initial_position.clone();
        let mut out = String::new();

        for m in &self.moves {
            let sfen_before = pos.to_sfen();
            if let Some(score) = m.effective_score() {
                // CSA→USI に変換して出力
                if let Ok(usi_mv) = csa_move_to_usi(&m.csa_move, &pos) {
                    writeln!(out, "{}\t{}\t{}", sfen_before, usi_mv, score).unwrap();
                }
            }
            if pos.apply_csa_move(&m.csa_move).is_err() {
                break;
            }
        }
        Ok(out)
    }
}

/// 棋譜をファイルに保存する
pub fn save_record(record: &GameRecord, config: &RecordConfig) -> Result<()> {
    save_record_with_status(record, config, &RecordStatus::Complete)
}

/// 棋譜種別を明示してファイルへ保存する。
pub fn save_record_with_status(
    record: &GameRecord,
    config: &RecordConfig,
    status: &RecordStatus,
) -> Result<()> {
    if !config.enabled {
        return Ok(());
    }

    std::fs::create_dir_all(&config.dir)?;

    let datetime = record.start_time.format("%Y%m%d_%H%M%S").to_string();
    let mut filename_base = config
        .filename_template
        .replace("{datetime}", &datetime)
        .replace("{game_id}", &record.game_id)
        .replace("{sente}", &sanitize_filename(&record.sente_name))
        .replace("{gote}", &sanitize_filename(&record.gote_name));
    filename_base.push_str(status.filename_suffix());

    if config.save_csa {
        let path = config.dir.join(format!("{filename_base}.csa"));
        std::fs::write(&path, record.to_csa_with_status(status))?;
        log::info!("[REC] 棋譜保存: {}", path.display());
    }

    if config.save_sfen {
        let sfen = record.to_sfen_lines()?;
        if !sfen.is_empty() {
            let path = config.dir.join(format!("{filename_base}.sfen"));
            std::fs::write(&path, sfen)?;
            log::info!("[REC] SFEN保存: {}", path.display());
        }
    }

    Ok(())
}

impl RecordStatus {
    /// 断片棋譜なら、全形式のファイル名に付ける suffix を返す。
    pub fn filename_suffix(&self) -> &'static str {
        match self {
            Self::Complete => "",
            Self::ReconnectFragment { .. } => "_reconnect_fragment",
        }
    }
}

fn opposite(color: Color) -> Color {
    match color {
        Color::Black => Color::White,
        Color::White => Color::Black,
    }
}

fn ms_to_sec(ms: i64) -> u64 {
    u64::try_from(ms).unwrap_or(0) / 1000
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(position: Position) -> GameSummary {
        GameSummary {
            game_id: "resume-game".to_owned(),
            my_color: Color::Black,
            sente_name: "alice".to_owned(),
            gote_name: "bob".to_owned(),
            position,
            initial_moves: Vec::new(),
            black_time: TimeConfig::default(),
            white_time: TimeConfig::default(),
            reconnect_token: Some("token".to_owned()),
            entering_king_rule: None,
        }
    }

    fn add_jsonl_placeholder(record: &mut GameRecord, sfen_before: String, usi: &str) {
        record.add_jsonl_move(JsonlMoveExtra {
            sfen_before,
            move_usi: usi.to_owned(),
            engine_label: "alice".to_owned(),
            elapsed_ms: 10,
            think_limit_ms: 100,
            seldepth: None,
            nodes: None,
            time_ms: None,
            nps: None,
        });
    }

    #[test]
    fn effective_score_omits_unrepresentable_white_min_value() {
        let mv = RecordedMove {
            csa_move: "-3334FU".to_owned(),
            time_sec: 1,
            eval_cp: Some(i32::MIN),
            eval_mate: None,
            depth: None,
            pv: Vec::new(),
            side_to_move: Color::White,
        };
        assert_eq!(mv.effective_score(), None);
    }

    #[test]
    fn resume_keeps_complete_record_when_position_matches() {
        let initial = rshogi_csa::initial_position();
        let mut retained = GameRecord::new(&summary(initial.clone()));
        let started_at = retained.start_time;

        let mut current = initial;
        let before = current.to_sfen();
        retained.add_move("+2726FU", 1, None, Color::Black);
        add_jsonl_placeholder(&mut retained, before, "2g2f");
        current.apply_csa_move("+2726FU").unwrap();
        let before = current.to_sfen();
        retained.add_move("-8384FU", 2, None, Color::White);
        add_jsonl_placeholder(&mut retained, before, "8c8d");
        current.apply_csa_move("-8384FU").unwrap();

        // Game_Summary の Position block は ply を運ばないため parse 後は 1 になる。
        let mut resume_position = current.clone();
        resume_position.ply = 1;
        let decision = retained.continue_for_resume(&summary(resume_position), Some(Color::Black));

        let ResumeRecordDecision::Continued {
            record,
            position,
            usi_moves,
        } = decision
        else {
            panic!("一致局面が fragment 扱いになりました");
        };
        assert_eq!(record.start_time, started_at);
        assert_eq!(record.moves.len(), 2);
        assert_eq!(record.jsonl_moves.len(), 2);
        assert_eq!(position.to_sfen(), current.to_sfen());
        assert_eq!(usi_moves, ["2g2f", "8c8d"]);
    }

    #[test]
    fn resume_falls_back_to_marked_fragment_when_position_mismatches() {
        let initial = rshogi_csa::initial_position();
        let mut retained = GameRecord::new(&summary(initial.clone()));
        let started_at = retained.start_time;
        retained.add_move("+2726FU", 1, None, Color::Black);

        let decision = retained.continue_for_resume(&summary(initial), Some(Color::Black));
        let ResumeRecordDecision::Fragment {
            record,
            retained_record,
            reason,
        } = decision
        else {
            panic!("不一致局面が通し棋譜として継続されました");
        };

        assert!(reason.contains("局面不一致"), "reason={reason}");
        assert!(record.moves.is_empty());
        assert_eq!(record.start_time, started_at);
        assert_eq!(retained_record.moves.len(), 1);
        let status = RecordStatus::ReconnectFragment { reason };
        assert_eq!(status.filename_suffix(), "_reconnect_fragment");
        let csa = record.to_csa_with_status(&status);
        assert!(
            csa.lines().nth(1).is_some_and(|line| line.starts_with("'RECONNECT_FRAGMENT:")),
            "CSA 先頭に断片コメントがありません:\n{csa}"
        );
    }
}
