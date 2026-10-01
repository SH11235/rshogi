//! SEE screening のスレッド別統計。通常 build には含めない。

use std::cell::Cell;

thread_local! {
    // R: 早期判定通過、G: 候補なし、E: pin 除外前の最初の相手攻め駒なし。
    static COUNTERS: Cell<[u64; 3]> = const { Cell::new([0; 3]) };
}

pub(crate) fn record(gate_empty: bool, first_empty: bool) {
    debug_assert!(!gate_empty || first_empty);
    COUNTERS.with(|cell| {
        let [r, g, e] = cell.get();
        cell.set([r + 1, g + u64::from(gate_empty), e + u64::from(first_empty)]);
    });
}

pub(crate) fn reset() {
    COUNTERS.set([0; 3]);
}

pub(crate) fn snapshot() -> [u64; 3] {
    COUNTERS.get()
}

pub(crate) fn format_report() -> String {
    let [r, g, e] = snapshot();
    format!("SEE opponent gate (current thread): R={r} G={g} E={e}\n")
}
