//! 共通のヒューリスティクス定数。
//!
//! MovePicker 周辺で利用する重み・更新係数を一元化し、ベンチでの
//! チューニングや setoption との連携を容易にする。

/// Quiet（Butterfly）ヒストリの最大値。
pub const QUIET_HISTORY_MAX: i16 = 32_000;
/// Quiet ヒストリ更新時の差分シフト（(bonus - value) >> shift）。
pub const QUIET_HISTORY_SHIFT: u32 = 5;
/// Quiet ヒストリのボーナス倍率（depth^2 * factor）。
pub const QUIET_HISTORY_BONUS_FACTOR: i32 = 32;
/// Quiet ヒストリのエイジング係数（value -= value >> AGING_SHIFT）。
pub const QUIET_HISTORY_AGING_SHIFT: u32 = 2;

/// Continuation ヒストリの最大値。
pub const CONT_HISTORY_MAX: i16 = 24_000;
/// Continuation ヒストリ更新時の差分シフト。
pub const CONT_HISTORY_SHIFT: u32 = 6;
/// Continuation ヒストリのボーナス倍率。
pub const CONT_HISTORY_BONUS_FACTOR: i32 = 24;
/// Continuation ヒストリのエイジング係数。
pub const CONT_HISTORY_AGING_SHIFT: u32 = 2;

/// Capture ヒストリの最大値。
pub const CAP_HISTORY_MAX: i16 = 32_000;
/// Capture ヒストリ更新時の差分シフト。
pub const CAP_HISTORY_SHIFT: u32 = 5;
/// Capture ヒストリのボーナス倍率。
pub const CAP_HISTORY_BONUS_FACTOR: i32 = 32;
/// Capture ヒストリのエイジング係数。
pub const CAP_HISTORY_AGING_SHIFT: u32 = 2;

/// Root quiet jitter amplitude (±value added to ordering key)
/// 値が大きいほど補助スレッドの探索順がバラけやすい。
pub const ROOT_JITTER_AMPLITUDE: i32 = 192;
