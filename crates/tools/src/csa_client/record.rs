//! 棋譜記録・保存

use std::fmt::Write as _;

use anyhow::Result;
use chrono::Local;

use crate::common::csa::{Color, Position, usi_move_to_csa};

use super::config::RecordConfig;
use super::engine::SearchInfo;
use super::protocol::GameSummary;

/// 対局中に蓄積する棋譜データ
#[derive(Clone, Debug)]
pub struct GameRecord {
    pub game_id: String,
    pub sente_name: String,
    pub gote_name: String,
    /// 先手の持ち時間（ミリ秒）
    pub black_total_time_ms: i64,
    /// 秒読み（ミリ秒）
    pub byoyomi_ms: i64,
    /// 対局開始時の局面
    pub initial_position: Position,
    pub moves: Vec<RecordedMove>,
    pub result: String,
    pub start_time: chrono::DateTime<Local>,
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

impl RecordedMove {
    /// 評価値を先手視点に正規化して返す。
    /// USI の score cp/mate は手番側視点なので、後手番なら符号を反転する。
    pub fn effective_score(&self) -> Option<i32> {
        let raw = if let Some(cp) = self.eval_cp {
            Some(cp)
        } else {
            self.eval_mate.map(|m| if m > 0 { 100000 } else { -100000 })
        };
        raw.map(|v| match self.side_to_move {
            Color::Black => v,
            Color::White => -v,
        })
    }
}

impl GameRecord {
    pub fn new(summary: &GameSummary) -> Self {
        Self {
            game_id: summary.game_id.clone(),
            sente_name: summary.sente_name.clone(),
            gote_name: summary.gote_name.clone(),
            black_total_time_ms: summary.black_time.total_time_ms,
            byoyomi_ms: summary.black_time.byoyomi_ms,
            initial_position: summary.position.clone(),
            moves: Vec::new(),
            result: String::new(),
            start_time: Local::now(),
        }
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

    pub fn set_result(&mut self, result: &str) {
        self.result = result.to_string();
    }

    /// CSA形式の棋譜テキストを生成する
    pub fn to_csa(&self) -> String {
        let mut out = String::new();
        writeln!(out, "V2.2").unwrap();
        writeln!(out, "N+{}", self.sente_name).unwrap();
        writeln!(out, "N-{}", self.gote_name).unwrap();
        writeln!(out, "$EVENT:{}", self.game_id).unwrap();
        writeln!(out, "$START_TIME:{}", self.start_time.format("%Y/%m/%d %H:%M:%S")).unwrap();
        let total_sec = (self.black_total_time_ms / 1000) as u32;
        let byoyomi_sec = (self.byoyomi_ms / 1000) as u32;
        writeln!(out, "$TIME_LIMIT:{}:{}+{:02}", total_sec / 60, total_sec % 60, byoyomi_sec)
            .unwrap();
        // 初期局面出力
        write!(out, "{}", self.initial_position.to_csa_board()).unwrap();
        writeln!(out).unwrap();

        // 盤面追跡（PV の USI→CSA 変換に使用）
        let mut pos = self.initial_position.clone();

        for m in &self.moves {
            // floodgate 形式コメント（評価値 + PV）
            if let Some(score) = m.effective_score() {
                write!(out, "'** {score}").unwrap();
                if !m.pv.is_empty() {
                    let mut pv_pos = pos.clone();
                    for usi_mv in &m.pv {
                        if let Ok(csa) = usi_move_to_csa(usi_mv, &pv_pos) {
                            write!(out, " {csa}").unwrap();
                            if pv_pos.apply_csa_move(&csa).is_err() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                writeln!(out).unwrap();
            }
            writeln!(out, "{}", m.csa_move).unwrap();
            writeln!(out, "T{}", m.time_sec).unwrap();
            let _ = pos.apply_csa_move(&m.csa_move);
        }

        // 終局コマンド
        match self.result.as_str() {
            "resign" => writeln!(out, "%TORYO").unwrap(),
            "win_declaration" => writeln!(out, "%KACHI").unwrap(),
            "sennichite" => writeln!(out, "%SENNICHITE").unwrap(),
            "time_up" => writeln!(out, "%TIME_UP").unwrap(),
            "illegal_move" => writeln!(out, "%ILLEGAL_MOVE").unwrap(),
            "jishogi" => writeln!(out, "%JISHOGI").unwrap(),
            "max_moves" => writeln!(out, "%MAX_MOVES").unwrap(),
            "interrupted" => writeln!(out, "%CHUDAN").unwrap(),
            // サーバーからの #WIN/#LOSE 結果（相手投了等で終局理由が不明な場合）
            "win" => writeln!(out, "'** result: win").unwrap(),
            "lose" => writeln!(out, "%TORYO").unwrap(), // 相手の勝ち = こちらの投了相当
            _ => {}
        }
        out
    }

    /// SFEN局面列を生成する（学習データ用）
    pub fn to_sfen_lines(&self) -> Result<String> {
        let mut pos = self.initial_position.clone();
        let mut out = String::new();

        for m in &self.moves {
            let sfen_before = pos.to_sfen();
            if let Some(score) = m.effective_score() {
                writeln!(out, "{}\t{}\t{}", sfen_before, m.csa_move, score).unwrap();
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
    if !config.enabled {
        return Ok(());
    }

    std::fs::create_dir_all(&config.dir)?;

    let datetime = record.start_time.format("%Y%m%d_%H%M%S").to_string();
    let filename_base = config
        .filename_template
        .replace("{datetime}", &datetime)
        .replace("{game_id}", &record.game_id)
        .replace("{sente}", &sanitize_filename(&record.sente_name))
        .replace("{gote}", &sanitize_filename(&record.gote_name));

    if config.save_csa {
        let path = config.dir.join(format!("{filename_base}.csa"));
        std::fs::write(&path, record.to_csa())?;
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
