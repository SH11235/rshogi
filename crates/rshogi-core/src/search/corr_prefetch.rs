//! 子局面で読む補正履歴のプリフェッチ。

use crate::position::Position;
use crate::types::Move;

use super::alpha_beta::SearchState;
use super::history::{CORRECTION_HISTORY_LIMIT, CorrectionHistory, HistoryCell, StatsEntry};

/// 着手直後の子局面に対応する局面キー4エントリと continuation 2エントリを先読みする。
#[inline]
pub(super) fn prefetch_correction(
    st: &SearchState,
    history: &HistoryCell,
    pos: &Position,
    child_ply: i32,
    mv: Move,
) {
    // SAFETY: 着手直後の単一探索スレッド内で、履歴の可変参照を保持していない。
    let h = unsafe { history.as_ref_unchecked() };
    for entry in h.correction_history.position_entries(pos) {
        prefetch_entry(entry);
    }
    for entry in continuation_entries(st, pos, child_ply, mv).into_iter().flatten() {
        prefetch_entry(entry);
    }
}

/// stack の current_move は着手後に設定される経路もあるため、今指した手を受け取る。
#[inline]
pub(super) fn continuation_entries(
    st: &SearchState,
    pos: &Position,
    child_ply: i32,
    mv: Move,
) -> [Option<*const StatsEntry<CORRECTION_HISTORY_LIMIT>>; 2] {
    if !mv.is_normal() {
        return [None; 2];
    }
    let to = mv.to();
    let pc = pos.piece_on(to);
    [2, 4].map(|back| {
        let index = usize::try_from(child_ply.checked_sub(back)?).ok()?;
        let table = st.stack.get(index)?.cont_correction_ptr;
        // SAFETY: 探索開始時に全 stack の pointer を生存中の履歴表/sentinel へ初期化し、
        // 着手直後には履歴の可変参照を保持しない。pc/to は子局面上の有効な駒/升。
        Some(unsafe { CorrectionHistory::continuation_entry_ptr(table, pc, to) })
    })
}

#[inline]
fn prefetch_entry<T>(entry: *const T) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: 呼出側が生存中の表内エントリを渡す。T0 prefetch は値を書き換えない。
    unsafe {
        use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
        _mm_prefetch(entry.cast(), _MM_HINT_T0);
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: 呼出側が生存中の表内エントリを渡し、TT と同じ読み取り prefetch を行う。
    unsafe {
        std::arch::aarch64::__prefetch(entry.cast());
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let _ = entry;
}
