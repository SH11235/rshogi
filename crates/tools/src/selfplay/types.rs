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

/// USI info 行から抽出した MultiPV 候補
#[derive(Debug, Clone)]
pub struct UsiMultiPvCandidate {
    /// MultiPV 番号（1-indexed）
    pub multipv: u32,
    /// 評価値（centipawns）
    pub score_cp: Option<i32>,
    /// 詰みスコア（手数）
    pub score_mate: Option<i32>,
    /// PV の先頭手（USI 文字列）
    pub first_move_usi: String,
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
    /// MultiPV 候補（multipv index で上書き）
    pub multipv_candidates: Vec<UsiMultiPvCandidate>,
}

impl InfoSnapshot {
    /// info 行を解析する。
    ///
    /// - multipv=1 の情報はメインフィールドを更新する
    /// - 全 multipv の候補を `multipv_candidates` に蓄積する（同一 multipv は上書き）
    pub fn update_from_line(&mut self, line: &str) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first().copied() != Some("info") {
            return;
        }

        // multipv 番号を抽出
        let mut multipv = 1u32;
        let mut idx = 1;
        while idx + 1 < tokens.len() {
            if tokens[idx] == "multipv" {
                multipv = tokens[idx + 1].parse::<u32>().unwrap_or(1);
                break;
            }
            idx += 1;
        }

        // score と pv を抽出（全 multipv 共通のパース）
        let mut score_cp: Option<i32> = None;
        let mut score_mate: Option<i32> = None;
        let mut depth: Option<u32> = None;
        let mut seldepth: Option<u32> = None;
        let mut nodes: Option<u64> = None;
        let mut time_ms: Option<u64> = None;
        let mut nps: Option<u64> = None;
        let mut pv: Vec<String> = Vec::new();

        let mut i = 1;
        while i < tokens.len() {
            match tokens[i] {
                "depth" if i + 1 < tokens.len() => {
                    depth = tokens[i + 1].parse::<u32>().ok();
                    i += 1;
                }
                "seldepth" if i + 1 < tokens.len() => {
                    seldepth = tokens[i + 1].parse::<u32>().ok();
                    i += 1;
                }
                "nodes" if i + 1 < tokens.len() => {
                    nodes = tokens[i + 1].parse::<u64>().ok();
                    i += 1;
                }
                "time" if i + 1 < tokens.len() => {
                    time_ms = tokens[i + 1].parse::<u64>().ok();
                    i += 1;
                }
                "nps" if i + 1 < tokens.len() => {
                    nps = tokens[i + 1].parse::<u64>().ok();
                    i += 1;
                }
                "score" if i + 2 < tokens.len() => match tokens[i + 1] {
                    "cp" => {
                        score_cp = tokens[i + 2].parse::<i32>().ok();
                        score_mate = None;
                        i += 2;
                    }
                    "mate" => {
                        score_mate = tokens[i + 2].parse::<i32>().ok();
                        score_cp = None;
                        i += 2;
                    }
                    _ => {}
                },
                "pv" => {
                    let mut j = i + 1;
                    while j < tokens.len() {
                        pv.push(tokens[j].to_string());
                        j += 1;
                    }
                    break;
                }
                _ => {}
            }
            i += 1;
        }

        // multipv=1 はメインフィールドも更新
        if multipv == 1 {
            if depth.is_some() {
                self.depth = depth;
            }
            if seldepth.is_some() {
                self.seldepth = seldepth;
            }
            if nodes.is_some() {
                self.nodes = nodes;
            }
            if time_ms.is_some() {
                self.time_ms = time_ms;
            }
            if nps.is_some() {
                self.nps = nps;
            }
            if score_cp.is_some() || score_mate.is_some() {
                self.score_cp = score_cp;
                self.score_mate = score_mate;
            }
            if !pv.is_empty() {
                self.pv = pv.clone();
            }
        }

        // MultiPV 候補として蓄積（PV がある場合のみ）
        if !pv.is_empty() && (score_cp.is_some() || score_mate.is_some()) {
            let candidate = UsiMultiPvCandidate {
                multipv,
                score_cp,
                score_mate,
                first_move_usi: pv[0].clone(),
            };
            if let Some(existing) =
                self.multipv_candidates.iter_mut().find(|c| c.multipv == multipv)
            {
                *existing = candidate;
            } else {
                self.multipv_candidates.push(candidate);
            }
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
    /// Some(n) の場合は `go depth n` を送信（byoyomiより優先）
    pub go_depth: Option<u32>,
    /// Some(n) の場合は `go nodes n` を送信
    pub go_nodes: Option<u64>,
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
    if color == Color::Black { 'b' } else { 'w' }
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

        // multipv=1 は multipv_candidates にも入る
        assert_eq!(snap.multipv_candidates.len(), 1);
        assert_eq!(snap.multipv_candidates[0].multipv, 1);
        assert_eq!(snap.multipv_candidates[0].score_cp, Some(34));
        assert_eq!(snap.multipv_candidates[0].first_move_usi, "7g7f");

        // multipv != 1 はメインフィールドを更新しないが候補には追加される
        snap.update_from_line("info multipv 2 depth 20 score cp 100 pv 2g2f");
        assert_eq!(snap.depth, Some(10)); // メインは変わらない
        assert_eq!(snap.multipv_candidates.len(), 2);
        assert_eq!(snap.multipv_candidates[1].multipv, 2);
        assert_eq!(snap.multipv_candidates[1].score_cp, Some(100));
        assert_eq!(snap.multipv_candidates[1].first_move_usi, "2g2f");
    }

    #[test]
    fn info_snapshot_multipv_overwrites_same_index() {
        let mut snap = InfoSnapshot::default();
        snap.update_from_line("info multipv 1 depth 10 score cp 50 pv 7g7f");
        snap.update_from_line("info multipv 2 depth 10 score cp 30 pv 2g2f");
        snap.update_from_line("info multipv 1 depth 11 score cp 55 pv 7g7f 3c3d");
        assert_eq!(snap.multipv_candidates.len(), 2);
        // multipv=1 は上書きされている
        assert_eq!(snap.multipv_candidates[0].score_cp, Some(55));
        // multipv=2 は変わらない
        assert_eq!(snap.multipv_candidates[1].score_cp, Some(30));
    }
}
