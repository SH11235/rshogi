//! 探索が静的評価した局面を確率で書き出す（`search-tree-sample` feature）。
//!
//! 静的評価を NNUE で実際に計算した局面（置換表の eval 流用・EvalHash hit を除く）を対象に、
//! 確率 `p` を引いてから「王手でない・駒得になる取る手と成る手が無い・静かな手がある」を
//! 判定し、満たした局面を固定容量のバッファに積む。探索中はヒープ割り当てをしない。
//! 標本は main thread の探索だけから取る（helper thread は書き出さない）。

use crate::movegen::{MoveList, generate_legal};
use crate::position::Position;
use crate::types::{Color, Hand, Piece, Square, Value};

/// 書き出しの設定。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TreeSampleConfig {
    /// 静的評価 1 回あたりの採択確率（0.0..=1.0）。
    pub probability: f64,
    /// 取り出す（`Search::drain_tree_samples`）までに積める標本の上限。超えた分は捨てて
    /// `TreeSampleStats::dropped_full` に数える。
    pub capacity: usize,
}

/// 書き出した局面。PSV へ詰めるのに必要な要素だけを持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeSample {
    /// マス index（0..81）順の駒。
    pub board: [Piece; 81],
    /// 手駒 [先手, 後手]。
    pub hands: [Hand; 2],
    /// 手番。
    pub side_to_move: Color,
    /// 玉のマス [先手, 後手]。
    pub king_squares: [Square; 2],
    /// 局面の手数（root の手数 + 探索の ply）。
    pub game_ply: i32,
}

impl TreeSample {
    fn from_position(pos: &Position) -> Self {
        let mut board = [Piece::NONE; 81];
        for (index, slot) in board.iter_mut().enumerate() {
            *slot = pos.piece_on(Square::from_u8(index as u8).expect("index < 81"));
        }
        Self {
            board,
            hands: [pos.hand(Color::Black), pos.hand(Color::White)],
            side_to_move: pos.side_to_move(),
            king_squares: [pos.king_square(Color::Black), pos.king_square(Color::White)],
            game_ply: pos.game_ply(),
        }
    }
}

/// 確率を引いた局面が条件のどこで落ちたかの累計。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TreeSampleStats {
    /// 確率に当たった局面数。
    pub drawn: u64,
    /// 王手で落ちた数。
    pub rejected_in_check: u64,
    /// SEE が正の取る手があって落ちた数。
    pub rejected_capture: u64,
    /// SEE が 0 以上の成る手があって落ちた数。
    pub rejected_promotion: u64,
    /// 静かな合法手が無くて落ちた数。
    pub rejected_no_quiet: u64,
    /// バッファが満杯で捨てた数。
    pub dropped_full: u64,
    /// バッファに積んだ数。
    pub accepted: u64,
}

impl TreeSampleStats {
    /// 別の累計を足し込む。
    pub fn merge(&mut self, other: &Self) {
        self.drawn += other.drawn;
        self.rejected_in_check += other.rejected_in_check;
        self.rejected_capture += other.rejected_capture;
        self.rejected_promotion += other.rejected_promotion;
        self.rejected_no_quiet += other.rejected_no_quiet;
        self.dropped_full += other.dropped_full;
        self.accepted += other.accepted;
    }
}

enum Quietness {
    InCheck,
    GainingCapture,
    GainingPromotion,
    NoQuietMove,
    Quiet,
}

fn classify(pos: &Position) -> Quietness {
    if pos.in_check() {
        return Quietness::InCheck;
    }
    let mut moves = MoveList::new();
    generate_legal(pos, &mut moves);
    let mut has_quiet = false;
    for &mv in moves.iter() {
        let capture = pos.is_capture(mv);
        if capture && pos.see_ge(mv, Value::new(1)) {
            return Quietness::GainingCapture;
        }
        // see_ge は成りの得を数えないので、取り返されない成りは駒得になる手として扱う。
        if mv.is_promotion() && pos.see_ge(mv, Value::ZERO) {
            return Quietness::GainingPromotion;
        }
        has_quiet |= !capture && !mv.is_promotion();
    }
    if has_quiet {
        Quietness::Quiet
    } else {
        Quietness::NoQuietMove
    }
}

/// 探索 1 本ぶんの乱数・バッファ・累計。
pub(crate) struct TreeSampler {
    threshold: u64,
    rng: u64,
    samples: Vec<TreeSample>,
    stats: TreeSampleStats,
}

impl TreeSampler {
    pub(crate) fn new(config: TreeSampleConfig, seed: u64) -> Self {
        assert!(
            (0.0..=1.0).contains(&config.probability),
            "tree sample probability must be in [0, 1]"
        );
        let threshold = if config.probability >= 1.0 {
            u64::MAX
        } else {
            (config.probability * (u64::MAX as f64 + 1.0)) as u64
        };
        Self {
            threshold,
            rng: seed,
            samples: Vec::with_capacity(config.capacity),
            stats: TreeSampleStats::default(),
        }
    }

    pub(crate) fn reseed(&mut self, seed: u64) {
        self.rng = seed;
    }

    fn next_u64(&mut self) -> u64 {
        // splitmix64
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 静的評価を計算した直後に呼ぶ。局面と探索状態は変更しない。
    #[inline]
    pub(crate) fn on_static_eval(&mut self, pos: &Position) {
        if self.threshold == 0 {
            return;
        }
        if self.next_u64() < self.threshold || self.threshold == u64::MAX {
            self.sample(pos);
        }
    }

    #[cold]
    fn sample(&mut self, pos: &Position) {
        self.stats.drawn += 1;
        match classify(pos) {
            Quietness::InCheck => self.stats.rejected_in_check += 1,
            Quietness::GainingCapture => self.stats.rejected_capture += 1,
            Quietness::GainingPromotion => self.stats.rejected_promotion += 1,
            Quietness::NoQuietMove => self.stats.rejected_no_quiet += 1,
            Quietness::Quiet => {
                if self.samples.len() < self.samples.capacity() {
                    self.samples.push(TreeSample::from_position(pos));
                    self.stats.accepted += 1;
                } else {
                    self.stats.dropped_full += 1;
                }
            }
        }
    }

    pub(crate) fn drain_into(&mut self, out: &mut Vec<TreeSample>) {
        out.extend_from_slice(&self.samples);
        self.samples.clear();
    }

    pub(crate) fn stats(&self) -> TreeSampleStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet_of(sfen: &str) -> bool {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        matches!(classify(&pos), Quietness::Quiet)
    }

    #[test]
    fn startpos_is_quiet() {
        assert!(quiet_of("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"));
    }

    #[test]
    fn hanging_piece_capture_is_not_quiet() {
        // 先手の飛車で只の後手の金を取れる。
        assert!(!quiet_of("4k4/9/9/9/4g4/9/9/9/4R3K b - 1"));
    }

    #[test]
    fn safe_promotion_is_not_quiet() {
        // 先手の歩が取り返されずに成れる。
        assert!(!quiet_of("k8/9/9/4P4/9/9/9/9/8K b - 1"));
    }

    #[test]
    fn in_check_is_not_quiet() {
        assert!(!quiet_of("4k4/9/9/9/4R4/9/9/9/8K w - 1"));
    }

    #[test]
    fn zero_probability_never_samples() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let mut sampler = TreeSampler::new(
            TreeSampleConfig {
                probability: 0.0,
                capacity: 4,
            },
            1,
        );
        for _ in 0..1000 {
            sampler.on_static_eval(&pos);
        }
        assert_eq!(sampler.stats(), TreeSampleStats::default());
    }

    #[test]
    fn full_buffer_drops_and_counts() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let mut sampler = TreeSampler::new(
            TreeSampleConfig {
                probability: 1.0,
                capacity: 2,
            },
            1,
        );
        for _ in 0..5 {
            sampler.on_static_eval(&pos);
        }
        let stats = sampler.stats();
        assert_eq!((stats.drawn, stats.accepted, stats.dropped_full), (5, 2, 3));
        let mut out = Vec::new();
        sampler.drain_into(&mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].game_ply, 1);
    }
}
