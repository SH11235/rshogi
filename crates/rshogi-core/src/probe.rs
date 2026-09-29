//! 同一バイナリで配置・prefetch を比較するための診断設定。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

static FT_LARGE_PAGES: AtomicBool = AtomicBool::new(false);
static EVAL_HASH_PREFETCH: AtomicU8 = AtomicU8::new(0);

#[cfg(all(test, windows))]
pub(crate) static FORCE_FT_HEAP: AtomicBool = AtomicBool::new(false);

/// 以後ロードする LayerStacks FT 重みで Windows Large Pages を試す。
pub fn set_ft_large_pages(enabled: bool) {
    FT_LARGE_PAGES.store(enabled, Ordering::Relaxed);
}

pub(crate) fn ft_large_pages() -> bool {
    FT_LARGE_PAGES.load(Ordering::Relaxed)
}

/// 子の EvalHash prefetch を指定する（0: 現行、1: 全停止、2: qsearch/ProbCut のみ停止）。
/// 範囲外の値は無視し、false を返す。
pub fn set_eval_hash_prefetch(mode: u8) -> bool {
    if mode > 2 {
        return false;
    }
    EVAL_HASH_PREFETCH.store(mode, Ordering::Relaxed);
    true
}

/// 呼び出し側で探索種別を固定し、追加コストを Relaxed load と分岐 1 個に留める。
#[inline]
pub(crate) fn prefetch_child<const TACTICAL: bool>() -> bool {
    let mode = EVAL_HASH_PREFETCH.load(Ordering::Relaxed);
    if TACTICAL { mode == 0 } else { mode != 1 }
}
