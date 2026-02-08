use rshogi_core::types::Color;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct EvalLog {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_cp: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_mate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seldepth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pv: Option<Vec<String>>,
}

#[derive(Default, Clone)]
pub struct InfoSnapshot {
    pub score_cp: Option<i32>,
    pub score_mate: Option<i32>,
    pub depth: Option<u32>,
    pub seldepth: Option<u32>,
    pub nodes: Option<u64>,
    pub time_ms: Option<u64>,
    pub nps: Option<u64>,
    pub pv: Vec<String>,
}

impl InfoSnapshot {
    /// info 行を解析し、multipv=1 の情報を保持する。
    pub fn update_from_line(&mut self, line: &str) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first().copied() != Some("info") {
            return;
        }
        let mut multipv = 1u32;
        let mut idx = 1;
        while idx + 1 < tokens.len() {
            if tokens[idx] == "multipv" {
                multipv = tokens[idx + 1].parse::<u32>().unwrap_or(1);
                break;
            }
            idx += 1;
        }
        if multipv != 1 {
            return;
        }
        let mut i = 1;
        while i < tokens.len() {
            match tokens[i] {
                "depth" => {
                    if i + 1 < tokens.len() {
                        self.depth = tokens[i + 1].parse::<u32>().ok();
                        i += 1;
                    }
                }
                "seldepth" => {
                    if i + 1 < tokens.len() {
                        self.seldepth = tokens[i + 1].parse::<u32>().ok();
                        i += 1;
                    }
                }
                "nodes" => {
                    if i + 1 < tokens.len() {
                        self.nodes = tokens[i + 1].parse::<u64>().ok();
                        i += 1;
                    }
                }
                "time" => {
                    if i + 1 < tokens.len() {
                        self.time_ms = tokens[i + 1].parse::<u64>().ok();
                        i += 1;
                    }
                }
                "nps" => {
                    if i + 1 < tokens.len() {
                        self.nps = tokens[i + 1].parse::<u64>().ok();
                        i += 1;
                    }
                }
                "score" => {
                    if i + 2 < tokens.len() {
                        match tokens[i + 1] {
                            "cp" => {
                                self.score_cp = tokens[i + 2].parse::<i32>().ok();
                                self.score_mate = None;
                                i += 2;
                            }
                            "mate" => {
                                self.score_mate = tokens[i + 2].parse::<i32>().ok();
                                self.score_cp = None;
                                i += 2;
                            }
                            _ => {}
                        }
                    }
                }
                "pv" => {
                    let mut pv = Vec::new();
                    let mut j = i + 1;
                    while j < tokens.len() {
                        pv.push(tokens[j].to_string());
                        j += 1;
                    }
                    if !pv.is_empty() {
                        self.pv = pv;
                    }
                    break;
                }
                _ => {}
            }
            i += 1;
        }
    }

    pub fn into_eval_log(self) -> Option<EvalLog> {
        if self.score_cp.is_none()
            && self.score_mate.is_none()
            && self.depth.is_none()
            && self.seldepth.is_none()
            && self.nodes.is_none()
            && self.time_ms.is_none()
            && self.nps.is_none()
            && self.pv.is_empty()
        {
            return None;
        }
        Some(EvalLog {
            score_cp: self.score_cp,
            score_mate: self.score_mate,
            depth: self.depth,
            seldepth: self.seldepth,
            nodes: self.nodes,
            time_ms: self.time_ms,
            nps: self.nps,
            pv: if self.pv.is_empty() {
                None
            } else {
                Some(self.pv)
            },
        })
    }
}

pub struct SearchRequest<'a> {
    pub sfen: &'a str,
    pub time_args: TimeArgs,
    pub think_limit_ms: u64,
    pub timeout_margin_ms: u64,
    pub game_id: u32,
    pub ply: u32,
    pub side: Color,
    pub engine_label: String,
    /// パス権利（先手, 後手）: Someの場合はpassrightsキーワードで送信
    pub pass_rights: Option<(u8, u8)>,
}

pub struct SearchOutcome {
    pub bestmove: Option<String>,
    pub elapsed_ms: u64,
    pub timed_out: bool,
    pub eval: Option<EvalLog>,
}

#[derive(Clone, Copy)]
pub struct TimeArgs {
    pub btime: u64,
    pub wtime: u64,
    pub byoyomi: u64,
    pub binc: u64,
    pub winc: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GameOutcome {
    InProgress,
    BlackWin,
    WhiteWin,
    Draw,
}

impl GameOutcome {
    pub fn label(self) -> &'static str {
        match self {
            GameOutcome::InProgress => "in_progress",
            GameOutcome::BlackWin => "black_win",
            GameOutcome::WhiteWin => "white_win",
            GameOutcome::Draw => "draw",
        }
    }
}

pub fn side_label(color: Color) -> char {
    if color == Color::Black {
        'b'
    } else {
        'w'
    }
}

pub fn duration_to_millis(d: Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

/// info コールバックの型エイリアス
pub type InfoCallback<'a> = dyn FnMut(&str, &SearchRequest<'_>) + 'a;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_snapshot_parses_primary_pv() {
        let mut snap = InfoSnapshot::default();
        snap.update_from_line(
            "info depth 10 seldepth 12 nodes 12345 time 67 nps 890 score cp 34 pv 7g7f 3c3d",
        );
        assert_eq!(snap.depth, Some(10));
        assert_eq!(snap.seldepth, Some(12));
        assert_eq!(snap.nodes, Some(12_345));
        assert_eq!(snap.time_ms, Some(67));
        assert_eq!(snap.nps, Some(890));
        assert_eq!(snap.score_cp, Some(34));
        assert_eq!(snap.score_mate, None);
        assert_eq!(snap.pv, vec!["7g7f".to_string(), "3c3d".to_string()]);

        // multipv != 1 は無視される
        snap.update_from_line("info multipv 2 depth 20 score cp 100 pv 2g2f");
        assert_eq!(snap.depth, Some(10));
    }
}
