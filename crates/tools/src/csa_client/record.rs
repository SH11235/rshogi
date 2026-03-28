//! 棋譜記録・保存

use std::fmt::Write as _;

use anyhow::Result;
use chrono::Local;

use crate::common::csa::Position;

use super::config::RecordConfig;
use super::engine::SearchInfo;
use super::protocol::GameSummary;

/// 対局中に蓄積する棋譜データ
#[derive(Clone, Debug)]
pub struct GameRecord {
    pub game_id: String,
    pub sente_name: String,
    pub gote_name: String,
    pub total_time_sec: u32,
    pub byoyomi_sec: u32,
    pub increment_sec: u32,
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
    pub depth: Option<u32>,
    pub pv: Vec<String>,
}

impl GameRecord {
    pub fn new(summary: &GameSummary) -> Self {
        Self {
            game_id: summary.game_id.clone(),
            sente_name: summary.sente_name.clone(),
            gote_name: summary.gote_name.clone(),
            total_time_sec: summary.total_time_sec,
            byoyomi_sec: summary.byoyomi_sec,
            increment_sec: summary.increment_sec,
            initial_position: summary.position.clone(),
            moves: Vec::new(),
            result: String::new(),
            start_time: Local::now(),
        }
    }

    pub fn add_move(&mut self, csa_move: &str, time_sec: u32, info: Option<&SearchInfo>) {
        let (eval_cp, depth, pv) = match info {
            Some(i) => (i.score_cp, i.depth, i.pv.clone()),
            None => (None, None, Vec::new()),
        };
        self.moves.push(RecordedMove {
            csa_move: csa_move.to_string(),
            time_sec,
            eval_cp,
            depth,
            pv,
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
        writeln!(
            out,
            "$TIME_LIMIT:{}:{}+{:02}",
            self.total_time_sec / 60,
            self.total_time_sec % 60,
            self.byoyomi_sec
        )
        .unwrap();
        // 初期局面出力
        write!(out, "{}", self.initial_position.to_csa_board()).unwrap();
        writeln!(out).unwrap();

        for m in &self.moves {
            // 評価値コメント
            if let Some(cp) = m.eval_cp {
                write!(out, "'** {cp}").unwrap();
                if let Some(d) = m.depth {
                    write!(out, " d{d}").unwrap();
                }
                writeln!(out).unwrap();
            }
            writeln!(out, "{}", m.csa_move).unwrap();
            writeln!(out, "T{}", m.time_sec).unwrap();
        }

        if self.result == "resign" {
            writeln!(out, "%TORYO").unwrap();
        } else if self.result == "win_declaration" {
            writeln!(out, "%KACHI").unwrap();
        }
        out
    }

    /// SFEN局面列を生成する（学習データ用）
    pub fn to_sfen_lines(&self) -> Result<String> {
        let mut pos = self.initial_position.clone();
        let mut out = String::new();

        for m in &self.moves {
            let sfen_before = pos.to_sfen();
            if let Some(cp) = m.eval_cp {
                writeln!(out, "{}\t{}\t{}", sfen_before, m.csa_move, cp).unwrap();
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
