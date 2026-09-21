//! 診断用バイナリ向けの割り当て分類。
//!
//! 実行ファイル側で [`record`] を呼ぶ allocator を登録する。分類はスレッド単位、
//! カウンタはプロセス全体。要求回数・要求サイズを数え、生存メモリ量は扱わない。

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Allocation call site category, independent of the allocation's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Phase {
    Other,
    Setup,
    Iteration,
    Tree,
    Output,
}

/// レポートの行順。
pub const PHASES: [Phase; 5] = [
    Phase::Other,
    Phase::Setup,
    Phase::Iteration,
    Phase::Tree,
    Phase::Output,
];

thread_local! {
    static PHASE: Cell<Phase> = const { Cell::new(Phase::Other) };
}

/// Restores the enclosing scope; must be dropped on its originating thread.
pub struct Scope {
    previous: Phase,
    _thread_bound: PhantomData<Rc<()>>,
}

impl Scope {
    /// 現在スレッドの分類を切り替え、drop 時に元へ戻す。
    pub fn enter(phase: Phase) -> Self {
        Self {
            previous: PHASE.with(|slot| slot.replace(phase)),
            _thread_bound: PhantomData,
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let _ = PHASE.try_with(|slot| slot.set(self.previous));
    }
}

/// 計測用 allocator が通知した確保・解放イベント。
#[derive(Clone, Copy)]
pub enum Operation {
    Alloc,
    Dealloc,
}

static COUNTERS: [[AtomicU64; 3]; 5] = [const { [const { AtomicU64::new(0) }; 3] }; 5];

/// Records a successful operation without allocation, locks, or formatting.
/// `bytes` は確保イベントの要求サイズ。解放はサイズの合計に加えない。
pub fn record(operation: Operation, bytes: usize) {
    let phase = PHASE.try_with(Cell::get).unwrap_or(Phase::Other) as usize;
    let index = match operation {
        Operation::Alloc => 0,
        Operation::Dealloc => 1,
    };
    COUNTERS[phase][index].fetch_add(1, Ordering::Relaxed);
    if !matches!(operation, Operation::Dealloc) {
        COUNTERS[phase][2].fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

/// Rows follow [`PHASES`]; columns: allocation events, deallocation events, object bytes.
/// Snapshots are not atomic across counters. Read after search workers have joined.
pub fn snapshot() -> [[u64; 3]; 5] {
    std::array::from_fn(|p| std::array::from_fn(|c| COUNTERS[p][c].load(Ordering::Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_restore_and_do_not_propagate_to_workers() {
        let _outer = Scope::enter(Phase::Setup);
        {
            let _inner = Scope::enter(Phase::Tree);
            assert_eq!(PHASE.with(Cell::get), Phase::Tree);
            assert_eq!(std::thread::spawn(|| PHASE.with(Cell::get)).join().unwrap(), Phase::Other);
        }
        assert_eq!(PHASE.with(Cell::get), Phase::Setup);
    }
}
