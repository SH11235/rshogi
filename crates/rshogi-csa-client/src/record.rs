//! 棋譜記録・保存

use std::fmt::Write as _;

use anyhow::Result;
use chrono::Local;

use rshogi_csa::{Color, Position, usi_move_to_csa};

use crate::config::RecordConfig;
use crate::engine::SearchInfo;
use crate::protocol::{GameSummary, TimeConfig};

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
    pub result: String,
    pub start_time: chrono::DateTime<Local>,
    /// 自エンジンの手番。JSONL 出力モードで `outcome` / `winner` の正規化に使う。
    pub my_color: Color,
    /// JSONL 出力モード用に蓄積する手単位の追加情報。CSA / SFEN 棋譜出力には影響しない。
    /// 各要素は `moves[i]` に対応する。投了 / 勝ち宣言など `apply_csa_move` を経由しない
    /// 手は含まれず、ply ベースで一致する。
    pub jsonl_moves: Vec<JsonlMoveExtra>,
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
    /// この手を指したエンジンのラベル。CSA 上のプレイヤー名 (`sente_name` / `gote_name`)
    /// と一致させるため、先手手番なら `sente_name`、後手手番なら `gote_name` を入れる。
    /// analyze_selfplay の per-engine timing 集計でこのラベルがキーになる。
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
            black_time: summary.black_time.clone(),
            white_time: summary.white_time.clone(),
            initial_position: summary.position.clone(),
            moves: Vec::new(),
            result: String::new(),
            start_time: Local::now(),
            my_color: summary.my_color,
            jsonl_moves: Vec::new(),
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
        // 先手の時間設定を $TIME_LIMIT に出力（CSA標準）
        let total_sec = (self.black_time.total_time_ms / 1000) as u32;
        let byoyomi_sec = (self.black_time.byoyomi_ms / 1000) as u32;
        let inc_sec = (self.black_time.increment_ms / 1000) as u32;
        if inc_sec > 0 {
            writeln!(out, "$TIME_LIMIT:{}:{:02}+{:02}F", total_sec / 60, total_sec % 60, inc_sec)
                .unwrap();
        } else {
            writeln!(
                out,
                "$TIME_LIMIT:{}:{:02}+{:02}",
                total_sec / 60,
                total_sec % 60,
                byoyomi_sec
            )
            .unwrap();
        }
        // 初期局面出力
        write!(out, "{}", self.initial_position.to_csa_board()).unwrap();
        writeln!(out).unwrap();

        // 盤面追跡（PV の USI→CSA 変換に使用）
        let mut pos = self.initial_position.clone();

        for m in &self.moves {
            // floodgate 形式コメント（評価値 + PV）
            if let Some(score) = m.effective_score() {
                write!(out, "'* {score}").unwrap();
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
            // サーバーからの #WIN/#LOSE/#DRAW（終局理由付きなら上書き済み）
            "win" => writeln!(out, "%TORYO").unwrap(), // 相手が投了した（こちらの勝ち）
            "lose" => writeln!(out, "%TORYO").unwrap(), // こちらが負けた
            _ => {}
        }
        out
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
