use smallvec::SmallVec;

use super::Heuristics;
use crate::movegen::MoveGenerator;
use crate::search::config;
use crate::search::params::{
    capture_history_weight, continuation_history_weight, quiet_history_weight, QS_PROMOTE_BONUS,
};
use crate::shogi::{Move, PieceType};
use crate::Position;
use std::cmp::Ordering;

/// Arguments for MovePicker::base to avoid too_many_arguments clippy warning
struct MovePickerArgs<'a> {
    pos: &'a Position,
    stage: Stage,
    tt_move: Option<Move>,
    excluded: Option<Move>,
    killers: [Option<Move>; 2],
    counter_move: Option<Move>,
    history_prev_move: Option<Move>,
    in_check: bool,
    qsearch_state: Option<QSearchState>,
    probcut_threshold: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Tt,
    GoodCaptures,
    Killers,
    GoodQuiets,
    BadQuiets,
    BadCaptures,
    Evasions,
    QGood,
    QChecks,
    QBad,
    ProbCut,
    Done,
}

impl Stage {
    #[cfg(any(test, debug_assertions, feature = "diagnostics"))]
    fn label(self) -> &'static str {
        match self {
            Stage::Tt => "tt",
            Stage::GoodCaptures => "good_captures",
            Stage::Killers => "killers",
            Stage::GoodQuiets => "good_quiets",
            Stage::BadQuiets => "bad_quiets",
            Stage::BadCaptures => "bad_captures",
            Stage::Evasions => "evasions",
            Stage::QGood => "q_good",
            Stage::QChecks => "q_checks",
            Stage::QBad => "q_bad",
            Stage::ProbCut => "probcut",
            Stage::Done => "done",
        }
    }
}

fn attacker_piece_type(pos: &Position, mv: Move) -> Option<PieceType> {
    mv.piece_type()
        .or_else(|| mv.from().and_then(|sq| pos.board.piece_on(sq)).map(|piece| piece.piece_type))
}

fn capture_value_plus_promote(pos: &Position, mv: Move) -> i32 {
    let mut value = 0;
    if let Some(captured) = pos.board.piece_on(mv.to()) {
        value += captured.capture_value();
    }
    if mv.is_promote() {
        if let Some(attacker) = attacker_piece_type(pos, mv) {
            value += attacker.promotion_gain();
        }
    }
    value
}

#[derive(Clone, Copy, Debug)]
struct ScoredMove {
    mv: Move,
    key: i32,
    tiebreak: u32,
}

#[derive(Clone, Copy, Debug)]
struct CaptureEntry {
    mv: Move,
    see: i32,
}

#[derive(Clone, Copy, Debug, Default)]
struct QSearchState {
    quiet_checks_generated: usize,
    quiet_check_limit: usize,
}

pub struct MovePicker<'a> {
    pos: &'a Position,
    stage: Stage,
    tt_move: Option<Move>,
    excluded: Option<Move>,
    killers: [Option<Move>; 2],
    counter_move: Option<Move>,
    history_prev_move: Option<Move>,
    in_check: bool,
    buf: SmallVec<[ScoredMove; 96]>,
    cursor: usize,
    used_tt: bool,
    killer_index: usize,
    qsearch_state: Option<QSearchState>,
    generated_captures: bool,
    capture_entries: SmallVec<[CaptureEntry; 64]>,
    generated_quiets: bool,
    quiet_moves: SmallVec<[Move; 96]>,
    deferred_bad_captures: SmallVec<[CaptureEntry; 32]>,
    returned: SmallVec<[u16; 128]>,
    probcut_threshold: Option<i32>,
    #[cfg(any(debug_assertions, feature = "diagnostics"))]
    epoch: u64,
}

impl<'a> MovePicker<'a> {
    pub fn new_normal(
        pos: &'a Position,
        tt_move: Option<Move>,
        excluded: Option<Move>,
        killers: [Option<Move>; 2],
        counter_move: Option<Move>,
        history_prev_move: Option<Move>,
    ) -> Self {
        let in_check = pos.is_in_check();
        Self::base(MovePickerArgs {
            pos,
            stage: if in_check { Stage::Evasions } else { Stage::Tt },
            tt_move,
            excluded,
            killers,
            counter_move,
            history_prev_move,
            in_check,
            qsearch_state: None,
            probcut_threshold: None,
        })
    }

    pub fn new_evasion(
        pos: &'a Position,
        tt_move: Option<Move>,
        excluded: Option<Move>,
        history_prev_move: Option<Move>,
    ) -> Self {
        Self::base(MovePickerArgs {
            pos,
            stage: Stage::Tt,
            tt_move,
            excluded,
            killers: [None, None],
            counter_move: None,
            history_prev_move,
            in_check: true,
            qsearch_state: None,
            probcut_threshold: None,
        })
    }

    pub fn new_qsearch(
        pos: &'a Position,
        tt_move: Option<Move>,
        excluded: Option<Move>,
        history_prev_move: Option<Move>,
        quiet_check_limit: usize,
    ) -> Self {
        let in_check = pos.is_in_check();
        let qs_state = Some(QSearchState {
            quiet_checks_generated: 0,
            quiet_check_limit,
        });
        Self::base(MovePickerArgs {
            pos,
            stage: if in_check { Stage::Evasions } else { Stage::Tt },
            tt_move,
            excluded,
            killers: [None, None],
            counter_move: None,
            history_prev_move,
            in_check,
            qsearch_state: qs_state,
            probcut_threshold: None,
        })
    }

    pub fn new_probcut(
        pos: &'a Position,
        excluded: Option<Move>,
        history_prev_move: Option<Move>,
        threshold: i32,
    ) -> Self {
        Self::base(MovePickerArgs {
            pos,
            stage: Stage::ProbCut,
            tt_move: None,
            excluded,
            killers: [None, None],
            counter_move: None,
            history_prev_move,
            in_check: pos.is_in_check(),
            qsearch_state: None,
            probcut_threshold: Some(threshold),
        })
    }

    fn base(args: MovePickerArgs<'a>) -> Self {
        let MovePickerArgs {
            pos,
            stage,
            tt_move,
            excluded,
            killers,
            counter_move,
            history_prev_move,
            in_check,
            qsearch_state,
            probcut_threshold,
        } = args;
        Self {
            pos,
            stage,
            tt_move,
            excluded,
            killers,
            counter_move,
            history_prev_move,
            in_check,
            buf: SmallVec::new(),
            cursor: 0,
            used_tt: false,
            killer_index: 0,
            qsearch_state,
            generated_captures: false,
            capture_entries: SmallVec::new(),
            generated_quiets: false,
            quiet_moves: SmallVec::new(),
            deferred_bad_captures: SmallVec::new(),
            returned: SmallVec::new(),
            probcut_threshold,
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            epoch: pos.state_epoch(),
        }
    }

    pub fn next(&mut self, heur: &Heuristics) -> Option<Move> {
        loop {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            {
                if self.epoch != self.pos.state_epoch() {
                    use crate::search::ab::diagnostics;
                    diagnostics::note_fault("move_picker_epoch_mismatch");
                    return None;
                }
            }
            match self.stage {
                Stage::Tt => {
                    self.advance_after_tt();
                    if let Some(mv) = self.pop_tt_if_legal() {
                        return Some(mv);
                    }
                    continue;
                }
                Stage::GoodCaptures => {
                    self.ensure_captures();
                    if self.cursor == 0 {
                        self.prepare_good_captures(heur);
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    self.transition(Stage::Killers);
                }
                Stage::Killers => {
                    if let Some(mv) = self.yield_killer_or_counter() {
                        return Some(mv);
                    }
                    self.transition(Stage::GoodQuiets);
                }
                Stage::GoodQuiets => {
                    if self.in_check {
                        self.transition(Stage::BadCaptures);
                        continue;
                    }
                    self.ensure_quiets();
                    if self.cursor == 0 {
                        self.prepare_good_quiets(heur);
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    self.transition(Stage::BadCaptures);
                }
                Stage::BadCaptures => {
                    self.ensure_captures();
                    if self.cursor == 0 {
                        self.prepare_bad_captures(heur);
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    self.transition(Stage::BadQuiets);
                }
                Stage::BadQuiets => {
                    if self.in_check {
                        self.transition(Stage::Done);
                        continue;
                    }
                    self.ensure_quiets();
                    if self.cursor == 0 {
                        self.prepare_bad_quiets(heur);
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    self.transition(Stage::Done);
                }
                Stage::Evasions => {
                    if self.cursor == 0 {
                        self.prepare_evasions(heur);
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    self.transition(if self.qsearch_state.is_some() {
                        Stage::Done
                    } else {
                        Stage::GoodCaptures
                    });
                }
                Stage::QGood => {
                    self.ensure_captures();
                    if self.cursor == 0 {
                        self.prepare_qs_captures(heur, true);
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    if self.qsearch_state.as_ref().is_some_and(|state| state.quiet_check_limit == 0)
                    {
                        self.transition(Stage::QBad);
                    } else {
                        self.transition(Stage::QChecks);
                    }
                }
                Stage::QChecks => {
                    self.ensure_quiets();
                    if self.cursor == 0 {
                        self.prepare_qs_checks(heur);
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    self.transition(Stage::QBad);
                }
                Stage::QBad => {
                    self.ensure_captures();
                    if self.cursor == 0 {
                        self.prepare_qs_captures(heur, false);
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    self.transition(Stage::Done);
                }
                Stage::ProbCut => {
                    if self.cursor == 0 {
                        self.prepare_probcut();
                    }
                    if let Some(mv) = self.pick_next() {
                        return Some(mv);
                    }
                    self.transition(Stage::Done);
                }
                Stage::Done => return None,
            }
        }
    }

    fn advance_after_tt(&mut self) {
        self.stage = if self.in_check {
            Stage::Evasions
        } else if self.qsearch_state.is_some() {
            Stage::QGood
        } else {
            Stage::GoodCaptures
        };
    }

    fn transition(&mut self, next: Stage) {
        self.stage = next;
        self.cursor = 0;
    }

    fn pop_tt_if_legal(&mut self) -> Option<Move> {
        if self.used_tt {
            return None;
        }
        if let Some(mv) = self.tt_move {
            if self.should_skip(mv) {
                self.used_tt = true;
                return None;
            }
            if self.targets_enemy_king(mv) {
                self.used_tt = true;
                return None;
            }
            if !self.pos.is_legal_move(mv) {
                self.used_tt = true;
                return None;
            }
            self.diag_guard(mv);
            self.used_tt = true;
            self.record_return(mv);
            return Some(mv);
        }
        None
    }

    fn ensure_captures(&mut self) {
        if self.generated_captures {
            return;
        }
        let mg = MoveGenerator::new();
        if let Ok(list) = mg.generate_captures(self.pos) {
            self.capture_entries = list
                .as_slice()
                .iter()
                .map(|&mv| CaptureEntry {
                    mv,
                    see: self.pos.see(mv),
                })
                .collect();
            self.deferred_bad_captures =
                self.capture_entries.iter().filter(|entry| entry.see < 0).copied().collect();
        }
        self.generated_captures = true;
    }

    fn ensure_quiets(&mut self) {
        if self.generated_quiets {
            return;
        }
        let mg = MoveGenerator::new();
        if let Ok(list) = mg.generate_quiet(self.pos) {
            self.quiet_moves = list.as_slice().iter().copied().collect();
        }
        self.generated_quiets = true;
    }

    fn prepare_good_captures(&mut self, heur: &Heuristics) {
        self.buf.clear();
        let capture_weight = capture_history_weight();
        for entry in &self.capture_entries {
            if entry.see < 0 {
                continue;
            }
            let mv = entry.mv;
            if self.should_skip(mv) {
                continue;
            }
            self.diag_guard(mv);
            // SEE加点は撤廃。CapturePieceValuePlusPromote(互換) + 履歴で並べ替え。
            let mut key = 2_000_000_i64;
            key += capture_value_plus_promote(self.pos, mv) as i64;
            if let (Some(attacker), Some(victim)) = (mv.piece_type(), mv.captured_piece_type()) {
                let cap_score = heur.capture.get(self.pos.side_to_move, attacker, victim, mv.to());
                key += (cap_score as i64) * (capture_weight as i64);
            }
            debug_assert!(key.abs() < 3_500_000, "good capture key overflow: {key}");
            self.buf.push(ScoredMove {
                mv,
                key: Self::clamp_key(key),
                tiebreak: mv.to_u32(),
            });
        }
        self.buf.sort_unstable_by(Self::cmp_scored);
    }

    fn prepare_bad_captures(&mut self, heur: &Heuristics) {
        self.buf.clear();
        let capture_weight = capture_history_weight();
        for entry in &self.deferred_bad_captures {
            let mv = entry.mv;
            if self.should_skip(mv) {
                continue;
            }
            self.diag_guard(mv);
            // SEE加点は撤廃。
            let mut key = 100_000_i64;
            key += capture_value_plus_promote(self.pos, mv) as i64;
            if let (Some(attacker), Some(victim)) = (mv.piece_type(), mv.captured_piece_type()) {
                let cap_score = heur.capture.get(self.pos.side_to_move, attacker, victim, mv.to());
                key += (cap_score as i64) * (capture_weight as i64);
            }
            debug_assert!(key.abs() < 3_500_000, "bad capture key overflow: {key}");
            self.buf.push(ScoredMove {
                mv,
                key: Self::clamp_key(key),
                tiebreak: mv.to_u32(),
            });
        }
        self.buf.sort_unstable_by(Self::cmp_scored);
    }

    fn good_quiet_threshold(heur: &Heuristics) -> i32 {
        // しきい値: 履歴の最大値に応じて動的決定。履歴が空なら極大にして全QuietをBad側へ。
        let hmax = heur.history.max_abs() as i32;
        if hmax > 0 {
            hmax / 2
        } else {
            i32::MAX
        }
    }

    fn prepare_good_quiets(&mut self, heur: &Heuristics) {
        self.buf.clear();
        let quiet_weight = quiet_history_weight();
        let continuation_weight = continuation_history_weight();
        let thresh = Self::good_quiet_threshold(heur);
        for &mv in &self.quiet_moves {
            if self.should_skip(mv) || mv.is_capture_hint() {
                continue;
            }
            let h = heur.history.get(self.pos.side_to_move, mv);
            if h < thresh {
                continue;
            }
            self.diag_guard(mv);
            let mut key = 1_000_000_i64 + (h as i64) * (quiet_weight as i64);
            // Promote preference for quiet moves（駒種依存・YO寄せ）
            if mv.is_promote() {
                if let Some(pt) = attacker_piece_type(self.pos, mv) {
                    let gain_cp = pt.promotion_gain() as i64; // cp差（R/Bは大、L/N/Sは小）
                    if gain_cp > 0 {
                        let bias = config::promote_bias_cp().clamp(0, 1000) as i64; // スケール係数（USI）
                        key += gain_cp * bias; // 例: 600cp*20 => +12,000
                    }
                }
            }
            if let Some(prev) = self.history_prev_move {
                if let Some(counter) = heur.counter.get(self.pos.side_to_move, prev) {
                    if counter.equals_without_piece_type(&mv) {
                        key += 60_000;
                    }
                }
                if let (Some(prev_piece), Some(curr_piece)) = (prev.piece_type(), mv.piece_type()) {
                    let cont_key = crate::search::history::ContinuationKey::new(
                        self.pos.side_to_move,
                        prev_piece as usize,
                        prev.to(),
                        prev.is_drop(),
                        curr_piece as usize,
                        mv.to(),
                        mv.is_drop(),
                    );
                    let cont_score = heur.continuation.get(cont_key);
                    key += (cont_score as i64) * (continuation_weight as i64);
                }
            }
            if self
                .killers
                .iter()
                .any(|k| k.is_some_and(|kk| kk.equals_without_piece_type(&mv)))
            {
                key += 50_000;
            }
            self.buf.push(ScoredMove {
                mv,
                key: Self::clamp_key(key),
                tiebreak: mv.to_u32(),
            });
        }
        self.buf.sort_unstable_by(Self::cmp_scored);
    }

    fn prepare_bad_quiets(&mut self, heur: &Heuristics) {
        self.buf.clear();
        let quiet_weight = quiet_history_weight();
        let continuation_weight = continuation_history_weight();
        let thresh = Self::good_quiet_threshold(heur);
        for &mv in &self.quiet_moves {
            if self.should_skip(mv) || mv.is_capture_hint() {
                continue;
            }
            let h = heur.history.get(self.pos.side_to_move, mv);
            if h >= thresh {
                continue;
            }
            self.diag_guard(mv);
            let mut key = 1_000_000_i64 + (h as i64) * (quiet_weight as i64);
            if mv.is_promote() {
                if let Some(pt) = attacker_piece_type(self.pos, mv) {
                    let gain_cp = pt.promotion_gain() as i64;
                    if gain_cp > 0 {
                        let bias = config::promote_bias_cp().clamp(0, 1000) as i64;
                        key += gain_cp * bias;
                    }
                }
            }
            if let Some(prev) = self.history_prev_move {
                if let Some(counter) = heur.counter.get(self.pos.side_to_move, prev) {
                    if counter.equals_without_piece_type(&mv) {
                        key += 60_000;
                    }
                }
                if let (Some(prev_piece), Some(curr_piece)) = (prev.piece_type(), mv.piece_type()) {
                    let cont_key = crate::search::history::ContinuationKey::new(
                        self.pos.side_to_move,
                        prev_piece as usize,
                        prev.to(),
                        prev.is_drop(),
                        curr_piece as usize,
                        mv.to(),
                        mv.is_drop(),
                    );
                    let cont_score = heur.continuation.get(cont_key);
                    key += (cont_score as i64) * (continuation_weight as i64);
                }
            }
            if self
                .killers
                .iter()
                .any(|k| k.is_some_and(|kk| kk.equals_without_piece_type(&mv)))
            {
                key += 50_000;
            }
            self.buf.push(ScoredMove {
                mv,
                key: Self::clamp_key(key),
                tiebreak: mv.to_u32(),
            });
        }
        self.buf.sort_unstable_by(Self::cmp_scored);
    }

    fn prepare_evasions(&mut self, heur: &Heuristics) {
        self.buf.clear();
        let mg = MoveGenerator::new();
        if let Ok(list) = mg.generate_evasions(self.pos) {
            for &mv in list.as_slice() {
                if self.should_skip(mv) {
                    continue;
                }
                self.diag_guard(mv);
                let mut key = 1_500_000_i64;
                // 逃れでも並べ替えはMVV + 履歴に限定
                if let Some(victim) = mv.captured_piece_type() {
                    key += match victim {
                        crate::shogi::PieceType::Pawn => 100,
                        crate::shogi::PieceType::Lance => 300,
                        crate::shogi::PieceType::Knight => 300,
                        crate::shogi::PieceType::Silver => 500,
                        crate::shogi::PieceType::Gold => 600,
                        crate::shogi::PieceType::Bishop => 800,
                        crate::shogi::PieceType::Rook => 900,
                        crate::shogi::PieceType::King => 0,
                    } as i64;
                }
                key += heur.history.get(self.pos.side_to_move, mv) as i64;
                debug_assert!(key.abs() < 3_000_000, "evasion key overflow: {key}");
                self.buf.push(ScoredMove {
                    mv,
                    key: Self::clamp_key(key),
                    tiebreak: mv.to_u32(),
                });
            }
        }
        self.buf.sort_unstable_by(Self::cmp_scored);
    }

    fn prepare_qs_captures(&mut self, heur: &Heuristics, good: bool) {
        self.buf.clear();
        let capture_weight = capture_history_weight();
        for entry in &self.capture_entries {
            if good && entry.see < 0 {
                continue;
            }
            if !good && entry.see >= 0 {
                continue;
            }
            let mv = entry.mv;
            if self.should_skip(mv) {
                continue;
            }
            self.diag_guard(mv);
            // SEE加点は撤廃
            let mut key = 1_800_000_i64;
            if self.pos.gives_check(mv) {
                key += 5_000;
            }
            if mv.is_promote() {
                key += QS_PROMOTE_BONUS as i64;
            }
            if let (Some(attacker), Some(victim)) = (mv.piece_type(), mv.captured_piece_type()) {
                let cap_score = heur.capture.get(self.pos.side_to_move, attacker, victim, mv.to());
                key += (cap_score as i64) * (capture_weight as i64);
            }
            debug_assert!(key.abs() < 3_500_000, "qsearch capture key overflow: {key}");
            self.buf.push(ScoredMove {
                mv,
                key: Self::clamp_key(key),
                tiebreak: mv.to_u32(),
            });
        }
        self.buf.sort_unstable_by(Self::cmp_scored);
    }

    fn prepare_qs_checks(&mut self, heur: &Heuristics) {
        self.buf.clear();
        if let Some(mut state) = self.qsearch_state.take() {
            for &mv in &self.quiet_moves {
                if state.quiet_checks_generated >= state.quiet_check_limit {
                    break;
                }
                if self.should_skip(mv) || !self.pos.gives_check(mv) {
                    continue;
                }
                self.diag_guard(mv);
                let key = 1_200_000_i64 + (heur.history.get(self.pos.side_to_move, mv) as i64);
                debug_assert!(key.abs() < 2_000_000, "qsearch quiet-check key overflow: {key}");
                self.buf.push(ScoredMove {
                    mv,
                    key: Self::clamp_key(key),
                    tiebreak: mv.to_u32(),
                });
                state.quiet_checks_generated += 1;
            }
            self.qsearch_state = Some(state);
        }
        self.buf.sort_unstable_by(Self::cmp_scored);
    }

    fn prepare_probcut(&mut self) {
        self.buf.clear();
        let Some(threshold) = self.probcut_threshold else {
            return;
        };
        let mg = MoveGenerator::new();
        if let Ok(list) = mg.generate_captures(self.pos) {
            for &mv in list.as_slice() {
                if self.should_skip(mv) {
                    continue;
                }
                let see = self.pos.see(mv);
                if see < threshold {
                    continue;
                }
                self.diag_guard(mv);
                // 並べ替えは MVV + 履歴（SEEはゲートにのみ使用）
                let mut key = 2_000_000_i64;
                if let Some(victim) = mv.captured_piece_type() {
                    key += match victim {
                        crate::shogi::PieceType::Pawn => 100,
                        crate::shogi::PieceType::Lance => 300,
                        crate::shogi::PieceType::Knight => 300,
                        crate::shogi::PieceType::Silver => 500,
                        crate::shogi::PieceType::Gold => 600,
                        crate::shogi::PieceType::Bishop => 800,
                        crate::shogi::PieceType::Rook => 900,
                        crate::shogi::PieceType::King => 0,
                    } as i64;
                }
                self.buf.push(ScoredMove {
                    mv,
                    key: Self::clamp_key(key),
                    tiebreak: mv.to_u32(),
                });
            }
        }
        self.buf.sort_unstable_by(Self::cmp_scored);
    }

    fn yield_killer_or_counter(&mut self) -> Option<Move> {
        let order = [self.killers[0], self.killers[1], self.counter_move];
        while self.killer_index < order.len() {
            let candidate = order[self.killer_index];
            self.killer_index += 1;
            let Some(mv) = candidate else {
                continue;
            };
            if self.should_skip(mv) {
                continue;
            }
            if mv.is_capture_hint() {
                continue;
            }
            if self.targets_enemy_king(mv) {
                continue;
            }
            if !self.pos.is_legal_move(mv) {
                continue;
            }
            self.diag_guard(mv);
            self.record_return(mv);
            return Some(mv);
        }
        None
    }

    fn pick_next(&mut self) -> Option<Move> {
        while self.cursor < self.buf.len() {
            let mv = self.buf[self.cursor].mv;
            self.cursor += 1;
            if self.should_skip(mv) {
                continue;
            }
            if self.targets_enemy_king(mv) {
                continue;
            }
            self.diag_guard(mv);
            if !self.pos.is_legal_move(mv) {
                continue;
            }
            self.record_return(mv);
            return Some(mv);
        }
        None
    }

    fn targets_enemy_king(&self, mv: Move) -> bool {
        if mv.is_drop() {
            return false;
        }
        if let Some(king_sq) = self.pos.board.king_square(self.pos.side_to_move.opposite()) {
            mv.to() == king_sq
        } else {
            false
        }
    }

    fn should_skip(&self, mv: Move) -> bool {
        // excluded は同一 from/to（昇成違い含む）を広く遮断して singular 等の挙動を保つ
        if self.excluded.is_some_and(|ex| Self::excluded_matches(ex, mv)) {
            return true;
        }
        if self.tt_move.is_some_and(|tt| tt.to_tt_key() == mv.to_tt_key() && self.used_tt) {
            return true;
        }
        let key = mv.to_tt_key();
        self.returned.contains(&key)
    }

    fn record_return(&mut self, mv: Move) {
        self.returned.push(mv.to_tt_key());
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        {
            use crate::search::ab::diagnostics;
            diagnostics::record_stage(self.stage.label());
            crate::search::ab::ordering::move_picker::diagnostics::
                warn_quiet_destination_enemy_king(self.pos, mv, self.stage);
        }
    }

    #[inline]
    fn cmp_scored(a: &ScoredMove, b: &ScoredMove) -> Ordering {
        // 同点時のみステージ生成時の 32bit キーを使って巡回順序を決定し、比較回数を抑制する。
        b.key.cmp(&a.key).then_with(|| a.tiebreak.cmp(&b.tiebreak))
    }

    #[inline]
    fn excluded_matches(ex: Move, mv: Move) -> bool {
        if ex.is_drop() {
            // Drop は同一駒種＋同一 to のみ遮断（別駒種は別プランとみなす）
            mv.is_drop() && ex.drop_piece_type() == mv.drop_piece_type() && ex.to() == mv.to()
        } else if mv.is_drop() {
            false
        } else {
            // Root とは異なり、内部の singular/exclusion 用には昇成・不成をまとめて除外する
            ex.from() == mv.from() && ex.to() == mv.to()
        }
    }

    #[inline]
    fn clamp_key(key: i64) -> i32 {
        key.clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }

    #[inline]
    #[cfg(any(debug_assertions, feature = "diagnostics"))]
    fn diag_guard(&self, mv: Move) {
        diagnostics::guard_enemy_king_capture(self.pos, mv, self.stage);
    }

    #[inline]
    #[cfg(not(any(debug_assertions, feature = "diagnostics")))]
    fn diag_guard(&self, _mv: Move) {}
}

#[cfg(any(test, debug_assertions, feature = "diagnostics"))]
mod diagnostics {
    use super::Stage;
    use crate::search::ab::diagnostics as ab_diag;
    use crate::shogi::{Move, PieceType};
    use crate::usi::{move_to_usi, position_to_sfen};
    use crate::Position;
    use log::warn;
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    fn should_panic_on_enemy_king_capture() -> bool {
        static PANIC_ON_GUARD: OnceLock<bool> = OnceLock::new();
        *PANIC_ON_GUARD.get_or_init(|| match crate::util::env_var("SHOGI_PANIC_ON_KING_CAPTURE") {
            Some(value) => {
                let normalized = value.trim().to_ascii_lowercase();
                !(normalized == "0"
                    || normalized == "false"
                    || normalized == "no"
                    || normalized == "off")
            }
            None => true,
        })
    }

    pub(super) fn guard_enemy_king_capture(pos: &Position, mv: Move, stage: Stage) {
        let king_sq = match pos.board.king_square(pos.side_to_move.opposite()) {
            Some(sq) => sq,
            None => return,
        };
        if mv.to() != king_sq {
            return;
        }

        static REPORTED: OnceLock<Mutex<HashSet<(u64, u32)>>> = OnceLock::new();
        let mut guard = REPORTED
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .expect("poisoned mutex");
        let key = (pos.hash, mv.to_u32());
        if !guard.insert(key) {
            return;
        }

        drop(guard);

        let sfen = position_to_sfen(pos);
        let mv_str = move_to_usi(&mv);
        ab_diag::dump("move_picker_guard", pos, Some(mv));
        warn!(
            "[move_picker] enemy king capture candidate detected: stage={} depth_ply={} move={} side={:?} sfen={}",
            stage.label(),
            pos.ply,
            mv_str,
            pos.side_to_move,
            sfen
        );
        ab_diag::note_fault("king_capture_detected");
        if should_panic_on_enemy_king_capture() {
            panic!(
                "MovePicker generated move capturing opponent king ({} at stage {})",
                mv_str,
                stage.label()
            );
        }
    }

    pub(super) fn warn_quiet_destination_enemy_king(pos: &Position, mv: Move, stage: Stage) {
        if mv.is_capture_hint() || mv.is_drop() {
            return;
        }
        let Some(piece) = pos.board.piece_on(mv.to()) else {
            return;
        };
        if piece.piece_type != PieceType::King || piece.color == pos.side_to_move {
            return;
        }

        static REPORTED: OnceLock<Mutex<HashSet<(u64, u32)>>> = OnceLock::new();
        let mut guard = REPORTED
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .expect("quiet king guard mutex poisoned");
        if !guard.insert((pos.hash, mv.to_u32())) {
            return;
        }
        drop(guard);

        let mv_str = move_to_usi(&mv);
        let sfen = position_to_sfen(pos);
        ab_diag::dump("quiet_enemy_king", pos, Some(mv));
        warn!(
            "[move_picker] quiet move targets enemy king square: stage={} depth_ply={} move={} side={:?} piece={:?} sfen={}",
            stage.label(),
            pos.ply,
            mv_str,
            pos.side_to_move,
            piece,
            sfen
        );
        ab_diag::note_fault("king_capture_detected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movegen::MoveGenerator;
    use crate::search::ab::ordering::Heuristics;
    use crate::shogi::{Color, Piece, PieceType};
    use crate::usi::{parse_usi_move, parse_usi_square};

    #[test]
    fn tt_move_returns_first_once() {
        let pos = Position::startpos();
        let tt = parse_usi_move("7g7f").expect("legal tt move");
        let mut picker = MovePicker::new_normal(&pos, Some(tt), None, [None, None], None, None);
        let heur = Heuristics::default();
        let first = picker.next(&heur).expect("expected tt move to be returned first");
        assert_eq!(first, tt);
        while let Some(mv) = picker.next(&heur) {
            assert!(!mv.equals_without_piece_type(&tt), "TT move must not repeat");
        }
    }

    #[test]
    fn tt_move_drop_differentiation() {
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        let dest = parse_usi_square("5e").unwrap();
        let pawn_drop = Move::drop(PieceType::Pawn, dest);
        let lance_drop = Move::drop(PieceType::Lance, dest);

        pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
        pos.hands[Color::Black as usize][PieceType::Lance.hand_index().unwrap()] = 1;

        let mut picker =
            MovePicker::new_normal(&pos, Some(pawn_drop), None, [None, None], None, None);
        let heur = Heuristics::default();

        let first = picker.next(&heur).expect("expected TT pawn drop to be returned first");
        assert_eq!(first, pawn_drop);

        let mut found_lance = false;
        while let Some(mv) = picker.next(&heur) {
            if mv == lance_drop {
                found_lance = true;
                break;
            }
        }

        assert!(
            found_lance,
            "lance drop should remain selectable despite TT pawn drop with same destination",
        );
    }

    #[test]
    fn evasion_moves_are_legal() {
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Rook, Color::White));

        assert!(pos.is_in_check());
        let heur = Heuristics::default();
        let mut picker = MovePicker::new_evasion(&pos, None, None, None);
        let mut count = 0;
        while let Some(mv) = picker.next(&heur) {
            assert!(pos.is_legal_move(mv), "evasion must be legal");
            count += 1;
        }
        assert!(count > 0, "expected at least one legal evasion");
    }

    #[test]
    fn continuation_history_prioritizes_quiet() {
        let pos = Position::startpos();
        let mg = MoveGenerator::new();
        let moves = mg.generate_all(&pos).unwrap();

        let quiet_moves: Vec<Move> =
            moves.as_slice().iter().copied().filter(|m| !m.is_capture_hint()).collect();
        assert!(quiet_moves.len() >= 3);
        let prev = quiet_moves[0];
        let preferred = quiet_moves[1];
        let alternative = quiet_moves[2];

        let mut heur = Heuristics::default();
        let key = crate::search::history::ContinuationKey::new(
            Color::Black,
            prev.piece_type().unwrap() as usize,
            prev.to(),
            prev.is_drop(),
            preferred.piece_type().unwrap() as usize,
            preferred.to(),
            preferred.is_drop(),
        );
        heur.continuation.update_good(key, 5);

        let mut picker = MovePicker::new_normal(&pos, None, None, [None, None], None, Some(prev));
        let first = picker.next(&heur).unwrap();
        assert!(first.equals_without_piece_type(&preferred));

        let rest: Vec<_> = std::iter::from_fn(|| picker.next(&heur)).collect();
        assert!(rest.iter().any(|mv| mv.equals_without_piece_type(&alternative)));
    }

    #[test]
    fn capture_history_prioritizes_capture() {
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(parse_usi_square("4g").unwrap(), Piece::new(PieceType::Pawn, Color::White));
        pos.board
            .put_piece(parse_usi_square("6g").unwrap(), Piece::new(PieceType::Pawn, Color::White));

        let mg = MoveGenerator::new();
        let captures = mg.generate_captures(&pos).unwrap();
        let left_capture = captures
            .as_slice()
            .iter()
            .copied()
            .find(|m| m.equals_without_piece_type(&parse_usi_move("5h4g").unwrap()))
            .unwrap();
        let right_capture = captures
            .as_slice()
            .iter()
            .copied()
            .find(|m| m.equals_without_piece_type(&parse_usi_move("5h6g").unwrap()))
            .unwrap();

        let mut heur = Heuristics::default();
        heur.capture.update_good(
            Color::Black,
            PieceType::Gold,
            PieceType::Pawn,
            left_capture.to(),
            5,
        );

        let mut picker = MovePicker::new_normal(&pos, None, None, [None, None], None, None);
        let first = picker.next(&heur).unwrap();
        assert!(first.equals_without_piece_type(&left_capture));

        let rest: Vec<_> = std::iter::from_fn(|| picker.next(&heur)).collect();
        assert!(rest.iter().any(|mv| mv.equals_without_piece_type(&right_capture)));
    }

    #[test]
    fn qsearch_quiet_check_limit_respected() {
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Rook, Color::Black));

        let mg = MoveGenerator::new();
        let quiet_moves = mg.generate_quiet(&pos).unwrap();
        let quiet_checks: Vec<_> = quiet_moves
            .as_slice()
            .iter()
            .copied()
            .filter(|mv| pos.gives_check(*mv))
            .collect();
        assert!(quiet_checks.len() >= 3, "expected multiple quiet checks for test setup");

        let heur = Heuristics::default();
        let mut picker = MovePicker::new_qsearch(&pos, None, None, None, 2);

        let mut returned_checks = 0;
        while let Some(mv) = picker.next(&heur) {
            if pos.gives_check(mv) {
                returned_checks += 1;
            }
        }

        assert_eq!(returned_checks, 2, "quiet check limit must cap returned moves");
    }

    #[test]
    fn regression_move_picker_no_enemy_king_capture() {
        let sfen = "ln1g1g2l/1r1sks3/ppppppnpp/6p2/9/3P1P3/PPP1P1PPP/3S3R1/LN1GKGSNL b b 7";
        let pos = Position::from_sfen(sfen).expect("valid SFEN");
        let their_king_sq = pos
            .board
            .king_square(pos.side_to_move.opposite())
            .expect("opponent king must exist");

        let mut picker = MovePicker::new_normal(&pos, None, None, [None, None], None, None);
        let heur = Heuristics::default();
        let mut offending = Vec::new();

        while let Some(mv) = picker.next(&heur) {
            if mv.to() == their_king_sq {
                offending.push(crate::usi::move_to_usi(&mv).to_string());
            }
        }

        assert!(
            offending.is_empty(),
            "move picker produced moves capturing enemy king: {:?}",
            offending
        );
    }
}
