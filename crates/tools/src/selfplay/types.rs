use rshogi_core::types::Color;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// USI score の上下界。None は確定評価を表す。
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScoreBound {
    /// 真の評価は報告値以上。
    Lowerbound,
    /// 真の評価は報告値以下。
    Upperbound,
}

/// 同一info行で報告された評価値。距離不明のmateを通常cpへ変換しない。
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryScore {
    /// centipawn値。
    Cp(i32),
    /// 符号付き詰み手数。
    Mate(i32),
    /// 勝ちの詰み報告。手数不明。
    MateWin,
    /// 負けの詰み報告。手数不明。
    MateLoss,
}

/// depth・評価値・PVが同一行に揃った、boundなしの主PV報告。
/// USIにはiteration完了通知がないため、iteration完了や最終bestmoveとの一致は保証しない。
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ExactPrimaryInfo {
    /// 採用行で報告された深さ。
    pub depth: u32,
    /// 採用行の評価値。上下界付きの行は採用しない。
    pub score: PrimaryScore,
    /// 採用行の非空PV。字句検証済みだが盤面上の合法性は保証しない。
    pub pv: Vec<String>,
    /// 他の行からfieldを補わず採用した原文。
    pub raw_line: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct EvalLog {
    /// fieldごとの進捗値と区別した、最後の同一行exact primary報告。旧ログには存在しない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exact_primary: Option<ExactPrimaryInfo>,
    /// 評価値の上下界。旧ログには存在しない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_bound: Option<ScoreBound>,
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
    /// 不完全な行、bound付き報告、secondary PVでは上書きしない。
    pub last_exact_primary: Option<ExactPrimaryInfo>,
    /// 最後の主 PV 評価値に付随する上下界。
    pub score_bound: Option<ScoreBound>,
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
        let tokens: Vec<&str> =
            line.split_whitespace().take_while(|token| *token != "string").collect();
        if tokens.first().copied() != Some("info") {
            return;
        }

        // multipv 番号を抽出
        let mut multipv = 1u32;
        let mut valid_multipv = true;
        let mut idx = 1;
        while idx + 1 < tokens.len() {
            if tokens[idx] == "multipv" {
                let parsed = tokens[idx + 1].parse::<u32>();
                valid_multipv = parsed.is_ok();
                multipv = parsed.unwrap_or(1);
                break;
            }
            idx += 1;
        }

        // score と pv を抽出（全 multipv 共通のパース）
        let mut score_seen = false;
        let mut primary_score = None;
        let mut score_bound = None;
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
                "lowerbound" => score_bound = Some(ScoreBound::Lowerbound),
                "upperbound" => score_bound = Some(ScoreBound::Upperbound),
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
                        score_seen = true;
                        score_cp = tokens[i + 2].parse::<i32>().ok();
                        primary_score = score_cp.map(PrimaryScore::Cp);
                        score_mate = None;
                        i += 2;
                    }
                    "mate" => {
                        score_seen = true;
                        score_mate = tokens[i + 2].parse::<i32>().ok();
                        primary_score = match tokens[i + 2] {
                            "+" => Some(PrimaryScore::MateWin),
                            "-" => Some(PrimaryScore::MateLoss),
                            _ => score_mate.map(PrimaryScore::Mate),
                        };
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

        // 分割行を結合して架空のdepth/score/PV組を作らない。
        let header_count = |key: &str| {
            tokens
                .iter()
                .take_while(|token| **token != "pv")
                .filter(|token| **token == key)
                .count()
        };
        if multipv == 1
            && valid_multipv
            && header_count("depth") == 1
            && header_count("score") == 1
            && header_count("multipv") <= 1
            // 不正な数値fieldの値としてparserに消費されたbound語も拒否する。
            && header_count("lowerbound") == 0
            && header_count("upperbound") == 0
            && score_bound.is_none()
            && !pv.is_empty()
            && pv.iter().all(|mv| {
                mv == "win"
                    || rshogi_core::types::Move::from_usi_strict(mv)
                        .is_some_and(|parsed| parsed != rshogi_core::types::Move::NONE)
            })
            && let (Some(depth), Some(score)) = (depth, primary_score)
        {
            self.last_exact_primary = Some(ExactPrimaryInfo {
                depth,
                score,
                pv: pv.clone(),
                raw_line: line.to_owned(),
            });
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
            if score_seen {
                self.score_bound = score_bound;
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
            last_exact_primary: self.last_exact_primary,
            score_bound: self.score_bound,
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
    /// 履歴の基点の SFEN。現在局面は moves を適用して復元する。
    pub sfen: &'a str,
    /// 基点からの合法手列（USI、空白区切り）。単独局面の探索では空文字列。
    pub moves: &'a str,
    pub time_args: TimeArgs,
    pub think_limit_ms: u64,
    pub timeout_margin_ms: u64,
    /// 時間制御なしの nodes/depth 探索の期限（1 手、正のミリ秒）。
    pub limit_only_timeout_ms: Option<u64>,
    pub game_id: u32,
    pub ply: u32,
    pub side: Color,
    pub engine_label: String,
    /// 基点のパス権利（先手, 後手）。moves の再生前に適用する。
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
    /// 運用上の探索期限を超過した結果。勝敗に使わない。
    pub watchdog_fired: bool,
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
    fn coherent_primary_is_not_assembled_from_progress_lines() {
        let mut snap = InfoSnapshot::default();
        let raw = "info depth 8 score cp 30 nodes 100 pv 7g7f 3c3d";
        snap.update_from_line(raw);
        let expected = snap.last_exact_primary.clone().unwrap();
        assert_eq!(expected.depth, 8);
        assert_eq!(expected.score, PrimaryScore::Cp(30));
        assert_eq!(expected.raw_line, raw);
        for line in [
            "info depth 9 nodes 200",
            "info depth 9 score cp 50 lowerbound pv 2g2f",
            "info depth 9 score cp -50 upperbound pv 2g2f",
            "info depth 9 score cp 50 nodes lowerbound pv 2g2f",
            "info depth 9 score cp 50 time upperbound pv 2g2f",
            "info depth 9 score cp 50 nps lowerbound pv 2g2f",
            "info depth 9 score cp 50 seldepth upperbound pv 2g2f",
            "info multipv 2 depth 10 score cp 90 pv 2g2f",
            "info score cp 17",
            "info pv 2g2f",
            "info string depth 99 score cp 100 pv 2g2f",
            "info multipv bad depth 99 score cp 100 pv 2g2f",
            "info multipv 1 multipv 2 depth 99 score cp 100 pv 2g2f",
            "info depth 99 score cp bad pv 2g2f",
            "info depth 99 score cp 100 pv 2g2fx",
            "info depth 99 score cp 100 pv",
            "info depth 99 depth 100 score cp 100 pv 2g2f",
            "bestmove 7g7f",
            "stop",
        ] {
            snap.update_from_line(line);
            assert_eq!(snap.last_exact_primary.as_ref(), Some(&expected), "{line}");
        }
        assert_eq!(snap.nodes, Some(200));
        let log = snap.into_eval_log().unwrap();
        let json = serde_json::to_string(&log).unwrap();
        let restored: EvalLog = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.last_exact_primary, Some(expected));
    }

    #[test]
    fn missing_and_mate_primary_records_remain_explicit() {
        let mut snap = InfoSnapshot::default();
        for line in ["info depth 8", "info score cp 30", "info pv 7g7f"] {
            snap.update_from_line(line);
        }
        assert!(snap.clone().into_eval_log().unwrap().last_exact_primary.is_none());
        let old: EvalLog = serde_json::from_str(r#"{"depth":8,"score_cp":30}"#).unwrap();
        assert!(old.last_exact_primary.is_none());
        assert!(serde_json::to_value(old).unwrap().get("last_exact_primary").is_none());
        for (token, expected) in [
            ("+3", PrimaryScore::Mate(3)),
            ("-3", PrimaryScore::Mate(-3)),
            ("+", PrimaryScore::MateWin),
            ("-", PrimaryScore::MateLoss),
        ] {
            snap.update_from_line(&format!("info depth 10 score mate {token} pv 7g7f"));
            assert_eq!(snap.last_exact_primary.as_ref().unwrap().score, expected);
        }
    }

    #[test]
    fn score_bounds_follow_primary_score_and_survive_log_roundtrip() {
        for (token, bound) in [
            ("lowerbound", ScoreBound::Lowerbound),
            ("upperbound", ScoreBound::Upperbound),
        ] {
            let mut snap = InfoSnapshot::default();
            snap.update_from_line(&format!("info score cp 0 {token} pv 7g7f"));
            snap.update_from_line("info nodes 100");
            snap.update_from_line("info multipv 2 score cp 10 pv 2g2f");
            snap.update_from_line("info string score cp 900");
            assert_eq!(snap.score_cp, Some(0));
            assert_eq!(snap.score_bound, Some(bound));
            let log = snap.clone().into_eval_log().unwrap();
            let json = serde_json::to_string(&log).unwrap();
            let log: EvalLog = serde_json::from_str(&json).unwrap();
            assert_eq!(log.score_bound, Some(bound));
            snap.update_from_line("info score mate -3 pv 7g7f");
            assert_eq!(snap.score_bound, None);
            assert_eq!(snap.score_cp, None);
            assert_eq!(snap.score_mate, Some(-3));
        }
        let old: EvalLog = serde_json::from_str(r#"{"score_cp":0}"#).unwrap();
        assert_eq!(old.score_bound, None);
        assert!(serde_json::to_value(old).unwrap().get("score_bound").is_none());
    }

    #[test]
    fn unparsed_score_clears_previous_exact_evaluation() {
        for score in ["mate +", "mate -", "cp invalid"] {
            let mut snap = InfoSnapshot::default();
            snap.update_from_line("info score cp 0 pv 7g7f");
            snap.update_from_line(&format!("info score {score}"));
            let eval = snap.into_eval_log().unwrap();
            assert_eq!(eval.score_cp, None);
            assert_eq!(eval.score_mate, None);
        }
    }

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
