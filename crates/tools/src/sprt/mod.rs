//! SPRT (Sequential Probability Ratio Test) コア。
//!
//! Pentanomial + 正規化 Elo + ITP 法による fishtest 互換の逐次確率比検定。
//! `tournament --sprt` および `analyze_selfplay --sprt` から共通利用する。
//!
//! 視点は **challenger (test engine)** 固定。`nelo1 = +5` は
//! 「test engine が base より +5 強い」ことを検定する意味になる。
//!
//! 参考:
//! - Michel Van den Bergh, "Normalized Elo Practical"
//!   <https://cantate.be/Fishtest/normalized_elo_practical.pdf>
//! - shogitest (`src/sprt.rs`, `src/stats.rs`)

pub mod decision;
pub mod llr;
pub mod meta;
pub mod penta;

pub use decision::{Decision, judge};
pub use llr::SprtParameters;
pub use meta::SprtMetaLog;
pub use penta::{GameSide, Penta};
