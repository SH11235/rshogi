use super::*;
use crate::types::{Color, Hand, Piece, Square};

type FtCandidate = Option<(usize, usize, usize)>;
thread_local! {
    static FT_CANDIDATES: Cell<[[FtCandidate; 3]; 2]> = const { Cell::new([[None; 3]; 2]) };
    static FT_PAIRED: Cell<[u8; 3]> = const { Cell::new([0; 3]) };
    static FT_TOTALS: Cell<[[usize; 2]; 3]> = const { Cell::new([[0; 2]; 3]) };
    static FT_FINNY: Cell<bool> = const { Cell::new(false) };
    static FT_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static RECENT: RefCell<[Option<RecentInput>; 4096]> = const { RefCell::new([None; 4096]) };
    static SERIAL: Cell<u64> = const { Cell::new(0) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct EvalInput {
    board: [Piece; 81],
    hands: [Hand; 2],
    side: Color,
}
impl EvalInput {
    fn new(pos: &Position) -> Self {
        Self {
            board: std::array::from_fn(|i| pos.piece_on(Square::from_u8(i as u8).unwrap())),
            hands: [pos.hand(Color::Black), pos.hand(Color::White)],
            side: pos.side_to_move(),
        }
    }
}
#[derive(Clone, Copy)]
struct RecentInput {
    key: u64,
    serial: u64,
    input: EvalInput,
}

pub(super) fn reset_nnue_diagnostics() {
    RECENT.with_borrow_mut(|entries| entries.fill(None));
    SERIAL.set(0);
    FT_ACTIVE.set(false);
    FT_FINNY.set(false);
    FT_CANDIDATES.set([[None; 3]; 2]);
    FT_PAIRED.set([0; 3]);
    FT_TOTALS.set([[0; 2]; 3]);
}

pub(crate) fn nnue_input(pos: &Position) {
    add(EVAL_NNUE, 1);
    record_input(pos.key(), EvalInput::new(pos));
}
fn record_input(key: u64, input: EvalInput) {
    let serial = SERIAL.get() + 1;
    SERIAL.set(serial);
    RECENT.with_borrow_mut(|entries| {
        let slot = &mut entries[key as usize & 4095];
        if let Some(old) = slot
            && serial - old.serial <= 4096
        {
            if old.key == key && old.input == input {
                add(EVAL_REPEAT, 1);
            } else {
                add(EVAL_COLLISION, 1);
            }
        }
        *slot = Some(RecentInput { key, serial, input });
    });
}

pub(crate) fn ft_candidate(p: usize, horizon: usize, candidate: FtCandidate) {
    let mut candidates = FT_CANDIDATES.get();
    candidates[p][horizon] = candidate;
    FT_CANDIDATES.set(candidates);
}
pub(crate) fn finny_pair(p: usize, rows: usize) {
    if !FT_ACTIVE.get() {
        return;
    }
    FT_FINNY.set(true);
    for (h, candidate) in FT_CANDIDATES.get()[p].iter().enumerate() {
        let base = FT_CF + h * 7;
        let Some((ancestor_rows, distance, dirty)) = *candidate else {
            add(base + 6, 1);
            continue;
        };
        add(base, 1);
        add(base + 1, rows as u64);
        add(base + 2, ancestor_rows as u64);
        add(base + 3, u64::from(ancestor_rows < rows));
        add(base + 4, distance as u64);
        add(base + 5, dirty as u64);
        let delta = rows as i32 - ancestor_rows as i32;
        add(FT_CF_DELTA + h * 9 + delta_bucket(delta), 1);
        let mut paired = FT_PAIRED.get();
        paired[h] |= 1 << p;
        FT_PAIRED.set(paired);
        let mut totals = FT_TOTALS.get();
        totals[h][0] += rows;
        totals[h][1] += ancestor_rows;
        FT_TOTALS.set(totals);
    }
}
fn delta_bucket(delta: i32) -> usize {
    match delta {
        ..=-9 => 0,
        -8..=-5 => 1,
        -4..=-2 => 2,
        -1 => 3,
        0 => 4,
        1 => 5,
        2..=4 => 6,
        5..=8 => 7,
        _ => 8,
    }
}
pub(super) fn begin_ft() {
    FT_ACTIVE.set(true);
    FT_FINNY.set(false);
    FT_CANDIDATES.set([[None; 3]; 2]);
    FT_PAIRED.set([0; 3]);
    FT_TOTALS.set([[0; 2]; 3]);
}
pub(super) fn end_ft() {
    add(FT_FINNY_EVALS, u64::from(FT_FINNY.get()));
    for h in 0..3 {
        let paired = FT_PAIRED.get()[h];
        if paired != 0 {
            add(FT_CF_EVAL + h * 2, 1);
            let totals = FT_TOTALS.get()[h];
            add(FT_CF_EVAL + h * 2 + 1, u64::from(totals[0] > totals[1]));
            add(FT_CF_SINGLE + h, u64::from(paired != 3));
            add(FT_CF_EQUAL + h, u64::from(totals[0] == totals[1]));
        }
    }
    FT_ACTIVE.set(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_inputs_require_exact_match_and_expire() {
        reset();
        let pos = Position::new();
        let input = EvalInput::new(&pos);
        record_input(7, input);
        record_input(7, input);
        let mut other = input;
        other.side = !other.side;
        record_input(7, other);
        record_input(4103, input);
        SERIAL.set(SERIAL.get() + 4096);
        record_input(4103, input);
        COUNTS.with_borrow(|c| {
            assert_eq!(c[EVAL_REPEAT], 1);
            assert_eq!(c[EVAL_COLLISION], 2);
        });
        reset();
        record_input(7, input);
        COUNTS.with_borrow(|c| assert_eq!(c[EVAL_REPEAT], 0));
    }
    #[test]
    fn finny_pairs_include_missing_and_signed_distribution() {
        reset();
        let request = NnueRequest::new();
        ft_candidate(0, 0, Some((2, 2, 3)));
        ft_candidate(1, 0, Some((8, 1, 1)));
        finny_pair(0, 6);
        finny_pair(1, 4);
        drop(request);
        COUNTS.with_borrow(|c| {
            assert_eq!(c[FT_CF], 2);
            assert_eq!(c[FT_CF + 1], 10);
            assert_eq!(c[FT_CF + 2], 10);
            assert_eq!(c[FT_CF + 3], 1);
            assert_eq!(c[FT_CF_DELTA + 6], 1);
            assert_eq!(c[FT_CF_DELTA + 2], 1);
            assert_eq!(c[FT_CF + 7 + 6], 2);
            assert_eq!(c[FT_CF_EVAL], 1);
            assert_eq!(c[FT_CF_EVAL + 1], 0);
            assert_eq!(c[FT_CF_EQUAL], 1);
        });
    }
}

#[cfg(feature = "nnue-threat")]
thread_local! {
    static THREAT_CURRENT_DISTANCE: Cell<usize> = const { Cell::new(1) };
}
#[cfg(feature = "nnue-threat")]
pub(crate) fn set_threat_distance(distance: usize) {
    THREAT_CURRENT_DISTANCE.set(distance);
}
#[cfg(feature = "nnue-threat")]
pub(crate) fn threat_geometry(changed: u32, sources: u32) {
    let distance = THREAT_CURRENT_DISTANCE.get();
    add(THREAT_CHANGED + distance - 1, u64::from(changed));
    add(THREAT_SOURCES + distance - 1, u64::from(sources));
}
#[cfg(feature = "nnue-threat")]
pub(crate) fn threat_physical_edge() {
    add(THREAT_PHYSICAL_EDGES + THREAT_CURRENT_DISTANCE.get() - 1, 1);
}
