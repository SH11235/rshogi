//! 探索深さ（Depth）

/// 探索深さ
pub type Depth = i32;

/// 最大探索深度
pub const MAX_PLY: Depth = 128;

/// 静止探索の深さ
pub const DEPTH_QS: Depth = 0;

/// 未探索を示す深さ
pub const DEPTH_UNSEARCHED: Depth = -2;

/// TT格納用オフセット
pub const DEPTH_ENTRY_OFFSET: Depth = -3;

// 定数間の関係をコンパイル時に検証する
const _: () = {
    assert!(MAX_PLY == 128);
    assert!(DEPTH_QS == 0);
    assert!(DEPTH_UNSEARCHED == -2);
    assert!(DEPTH_ENTRY_OFFSET == -3);
    assert!(MAX_PLY > DEPTH_QS);
    assert!(DEPTH_QS > DEPTH_UNSEARCHED);
    assert!(DEPTH_UNSEARCHED > DEPTH_ENTRY_OFFSET);
};
