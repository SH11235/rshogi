//! Core SEE algorithm implementation
//!
//! This module implements the main Static Exchange Evaluation (SEE) algorithm
//! for evaluating capture sequences and determining whether a capture is likely
//! to be profitable.

use crate::shogi::board::Color;
use crate::shogi::moves::Move;
use crate::shogi::piece_constants::{SEE_GAIN_ARRAY_SIZE, SEE_MAX_DEPTH};
use crate::shogi::position::Position;

impl Position {
    /// Static Exchange Evaluation (SEE)
    /// Evaluates the material gain/loss from a capture sequence
    /// Returns the expected material gain from the move (positive = good, negative = bad)
    pub fn see(&self, mv: Move) -> i32 {
        self.see_internal(mv, 0)
    }

    /// Static Exchange Evaluation (no inline version for benchmarking)
    /// This prevents constant folding in benchmarks
    #[inline(never)]
    pub fn see_noinline(&self, mv: Move) -> i32 {
        self.see_internal(mv, 0)
    }

    /// Static Exchange Evaluation with threshold
    /// Returns true if the SEE value is greater than or equal to the threshold
    pub fn see_ge(&self, mv: Move, threshold: i32) -> bool {
        // Use threshold in internal calculation for early termination
        self.see_internal(mv, threshold) >= threshold
    }

    /// Internal SEE implementation using gain array algorithm
    /// Returns the expected material gain from the move
    /// If threshold is provided and SEE value cannot reach it, returns early
    fn see_internal(&self, mv: Move, threshold: i32) -> i32 {
        let to = mv.to();

        // Not a capture
        let captured = match self.board.piece_on(to) {
            Some(piece) => piece,
            None => return 0,
        };

        let captured_value = Self::see_piece_value(captured);

        // For drops, we assume the piece is safe
        if mv.is_drop() {
            return captured_value;
        }

        let from = mv.from().expect("Normal move must have from square");
        let mut occupied = self.board.all_bb;

        // Get the initial attacker
        let attacker = self.board.piece_on(from).expect("Move source must have a piece");
        let attacker_value = if mv.is_promote() {
            Self::see_promoted_piece_value(attacker.piece_type)
        } else {
            Self::see_piece_type_value(attacker.piece_type)
        };

        // 初手のピン合法性を確認（違法なら早期リターン）
        {
            let initial_pins = self.calculate_pins_for_color(self.side_to_move);
            if !initial_pins.can_move(from, to) {
                // 閾値比較を考慮し、到達不能にする十分小さい値を返す
                return if threshold != 0 {
                    threshold - 1
                } else {
                    -10_000
                };
            }
        }

        // プロモーション加点（初手の利得に昇格価値差を反映）
        let promotion_bonus = if mv.is_promote() {
            Self::see_promoted_piece_value(attacker.piece_type)
                - Self::see_piece_type_value(attacker.piece_type)
        } else {
            0
        };

        // Delta pruning optimization for SEE
        //
        // Returns early if the maximum possible gain cannot reach the threshold.
        // This optimization is particularly effective for:
        // - High thresholds that are clearly unreachable
        // - Shallow exchanges (2-4 captures)
        // - Positions with limited attacking pieces
        //
        // Only apply for see_ge calls (threshold != 0)
        if threshold != 0 && (captured_value + promotion_bonus) < threshold {
            // Best case is just capturing the target piece + promotion bonus
            return captured_value + promotion_bonus;
        }

        // Calculate pin information for both colors
        let (black_pins, white_pins) = self.calculate_pins_for_see();

        // Gain array to track material balance at each ply
        let mut gain = [0i32; SEE_GAIN_ARRAY_SIZE];
        let mut depth = 0;

        // Track cumulative evaluation to avoid O(n²) recalculation
        let mut cumulative_eval = captured_value + promotion_bonus;

        // gain[0] is the initial capture value (+昇格加点)
        gain[0] = captured_value + promotion_bonus;

        // Make the initial capture
        occupied.clear(from);
        occupied.set(to); // The capturing piece now occupies the target square

        // Get all attackers
        let mut attackers = self.get_all_attackers_to(to, occupied);

        let mut stm = self.side_to_move.opposite();
        // The first piece to be potentially recaptured is the initial attacker
        let mut _last_captured_value = attacker_value;

        // Generate capture sequence
        loop {
            // Select appropriate pin info based on side to move
            let pin_info = match stm {
                Color::Black => &black_pins,
                Color::White => &white_pins,
            };

            // Get next attacker considering pin constraints
            match self.pop_least_valuable_attacker_with_pins(
                &mut attackers,
                occupied,
                stm,
                to,
                pin_info,
            ) {
                Some((sq, _, attacker_value)) => {
                    depth += 1;

                    // gain[d] = 取られた駒の価値 - gain[d‑1]
                    gain[depth] = _last_captured_value - gain[depth - 1];

                    // Update cumulative evaluation (O(1) instead of O(n))
                    cumulative_eval = std::cmp::max(-cumulative_eval, gain[depth]);

                    // Delta pruning: early termination if we can't possibly reach threshold
                    if threshold != 0 && depth >= 1 {
                        // Current evaluation from initial side's perspective
                        let current_eval = if depth & 1 == 1 {
                            -cumulative_eval
                        } else {
                            cumulative_eval
                        };

                        // Estimate maximum possible remaining value
                        // Consider remaining attackers by piece type
                        let max_remaining_value = self.estimate_max_remaining_value(
                            &attackers,
                            stm,
                            threshold,
                            current_eval,
                        );

                        // Maximum possible gain
                        let max_possible_gain = if stm == self.side_to_move {
                            // We move next, can potentially gain more
                            current_eval + max_remaining_value
                        } else {
                            // Opponent moves next, our position might get worse
                            current_eval
                        };

                        // Early termination if we can't reach threshold
                        if max_possible_gain < threshold {
                            return current_eval;
                        }
                    }

                    // 深さの上限チェック
                    if depth >= SEE_MAX_DEPTH {
                        break;
                    }

                    // 盤面を更新
                    occupied.clear(sq); // 攻撃駒を元の升から除去
                    occupied.set(to); // 取った駒が目的地に移動
                    _last_captured_value = attacker_value; // 次に取られる駒の価値を更新

                    // X-ray を更新して「幽霊駒」問題を防ぐ
                    self.update_xray_attacks(sq, to, &mut attackers, occupied);

                    // 手番を反転
                    stm = stm.opposite();
                }
                None => break,
            }
        }

        // Apply negamax propagation from the end

        // Propagate scores from the end
        // At odd depths (1, 3, 5...), the opponent moved last, so we negate and maximize
        // At even depths (0, 2, 4...), we moved last, so we keep the sign

        // Work backwards, alternating between minimizing and maximizing
        for d in (0..depth).rev() {
            let _old_gain = gain[d];

            // Check who moved at this depth
            // depth 0: initial attacker (same as side_to_move)
            // depth 1: opponent
            // depth 2: initial attacker again
            // etc.

            // At each depth, we're computing from the perspective of who moved at that depth
            // They choose between standing pat (-gain[d]) or opponent's best continuation (gain[d+1])
            gain[d] = std::cmp::max(-gain[d], gain[d + 1]);
        }

        // Fix sign for even number of exchanges
        // When depth is odd (meaning even number of total exchanges),
        // the last move was made by the opponent, so we need to negate
        if depth & 1 == 1 {
            gain[0] = -gain[0];
        }

        gain[0]
    }
}
