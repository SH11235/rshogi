//! Position hasher for SFEN positions using Zobrist hashing
//!
//! This module provides efficient hashing of Shogi positions for use in the
//! opening book binary format. Uses Zobrist hashing for good distribution
//! and collision avoidance.

use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// Statistics about hashing performance
#[derive(Debug, Clone)]
pub struct HashStatistics {
    pub total_positions: usize,
    pub unique_positions: usize,
    pub collision_count: usize,
}

/// Position hasher using Zobrist hashing
pub struct PositionHasher {
    /// Zobrist hash tables for pieces at each square
    zobrist_pieces: [[u64; 32]; 81], // 81 squares, 32 piece types (including promoted)
    /// Zobrist hash for turn (future use)
    #[allow(dead_code)]
    zobrist_turn: u64,
    /// Zobrist hash for hand pieces (future use)
    #[allow(dead_code)]
    zobrist_hand: [u64; 14], // 7 piece types * 2 players
    /// Tracked positions for collision detection
    position_tracker: HashMap<String, u64>,
    /// Statistics
    stats: HashStatistics,
}

impl PositionHasher {
    /// Create a new position hasher
    pub fn new() -> Self {
        // Initialize Zobrist tables with pseudo-random values
        let mut zobrist_pieces = [[0u64; 32]; 81];
        let mut rng_state = 0x123456789abcdef0u64;

        // Fill piece tables
        for square_pieces in &mut zobrist_pieces {
            for piece_value in square_pieces {
                *piece_value = Self::next_random(&mut rng_state);
            }
        }

        let zobrist_turn = Self::next_random(&mut rng_state);

        let mut zobrist_hand = [0u64; 14];
        for value in &mut zobrist_hand {
            *value = Self::next_random(&mut rng_state);
        }

        Self {
            zobrist_pieces,
            zobrist_turn,
            zobrist_hand,
            position_tracker: HashMap::new(),
            stats: HashStatistics {
                total_positions: 0,
                unique_positions: 0,
                collision_count: 0,
            },
        }
    }

    /// Hash a position (static method)
    pub fn hash_position(position: &str) -> Result<u64> {
        let hasher = Self::new();
        hasher.hash_sfen_position(position)
    }

    /// Hash a position and track for collision detection
    pub fn hash_and_track(&mut self, position: &str) -> Result<u64> {
        let hash = self.hash_sfen_position(position)?;

        self.stats.total_positions += 1;

        if let Some(&existing_hash) = self.position_tracker.get(position) {
            // Same position seen before
            if existing_hash != hash {
                self.stats.collision_count += 1;
            }
        } else {
            // New unique position
            self.position_tracker.insert(position.to_string(), hash);
            self.stats.unique_positions += 1;
        }

        Ok(hash)
    }

    /// Get hashing statistics
    pub fn get_statistics(&self) -> HashStatistics {
        self.stats.clone()
    }

    /// Hash a SFEN position string
    fn hash_sfen_position(&self, position: &str) -> Result<u64> {
        let parts: Vec<&str> = position.split_whitespace().collect();

        if parts.is_empty() {
            return Err(anyhow!("Empty position"));
        }

        // Need at least board, turn, and hands
        if parts.len() < 3 {
            return Err(anyhow!("Invalid SFEN: expected at least 3 parts, got {}", parts.len()));
        }

        let board = parts[0];
        // let turn = parts[1];
        let hands = parts[2];

        #[cfg(target_arch = "wasm32")]
        {
            web_sys::console::log_1(
                &format!(
                    "[Rust PositionHasher] Parts: board={}, turn={}, hands={}, move_count={}",
                    board,
                    turn,
                    hands,
                    parts.get(3).unwrap_or(&"none")
                )
                .into(),
            );
        }

        // Start with board position hash
        let mut hash = self.hash_board_position(board)?;

        // 手番によらず、盤面・手駒だけでハッシュを計算するので十分
        // match turn {
        //     "b" => {
        //         // 先手の場合は基準値なので追加のXORは不要
        //     }
        //     "w" => {
        //         // 後手の場合は手番を区別するためのハッシュ値をXOR
        //         hash ^= self.zobrist_turn;
        //     }
        //     _ => return Err(anyhow!("Invalid turn: {}", turn)),
        // }

        // XOR with hands hash
        hash ^= self.hash_hands(hands)?;

        #[cfg(target_arch = "wasm32")]
        {
            web_sys::console::log_1(
                &format!("[Rust PositionHasher] Generated hash: {:#016x}", hash).into(),
            );
        }

        Ok(hash)
    }

    /// Hash the board position part of SFEN
    fn hash_board_position(&self, board_position: &str) -> Result<u64> {
        let ranks: Vec<&str> = board_position.split('/').collect();

        if ranks.len() != 9 {
            return Err(anyhow!("Invalid rank count: expected 9, got {}", ranks.len()));
        }

        let mut hash = 0u64;

        for (rank_idx, rank) in ranks.iter().enumerate() {
            hash ^= self.hash_rank(rank, rank_idx)?;
        }

        Ok(hash)
    }

    /// Hash a single rank
    fn hash_rank(&self, rank: &str, rank_idx: usize) -> Result<u64> {
        let mut hash = 0u64;
        let mut file_idx = 0;
        let mut chars = rank.chars().peekable();

        while let Some(ch) = chars.next() {
            if file_idx >= 9 {
                return Err(anyhow!("Rank too long: {}", rank));
            }

            if ch.is_ascii_digit() {
                // Empty squares
                let empty_count = ch.to_digit(10).unwrap() as usize;
                if file_idx + empty_count > 9 {
                    return Err(anyhow!("Invalid empty square count in rank: {}", rank));
                }
                file_idx += empty_count;
            } else {
                // Piece
                let (piece_type, promoted) = if ch == '+' {
                    // Promoted piece
                    if let Some(next_ch) = chars.next() {
                        (next_ch, true)
                    } else {
                        return Err(anyhow!("Invalid promoted piece in rank: {}", rank));
                    }
                } else {
                    (ch, false)
                };

                let piece_index = self.get_piece_index(piece_type, promoted)?;
                let square_index = rank_idx * 9 + file_idx;

                hash ^= self.zobrist_pieces[square_index][piece_index];
                file_idx += 1;
            }
        }

        if file_idx != 9 {
            return Err(anyhow!("Rank too short: expected 9 squares, got {}", file_idx));
        }

        Ok(hash)
    }

    /// Get piece index for Zobrist table
    fn get_piece_index(&self, piece_type: char, promoted: bool) -> Result<usize> {
        let base_index = match piece_type.to_ascii_uppercase() {
            'P' => 0, // Pawn
            'L' => 1, // Lance
            'N' => 2, // Knight
            'S' => 3, // Silver
            'G' => 4, // Gold
            'B' => 5, // Bishop
            'R' => 6, // Rook
            'K' => 7, // King
            _ => return Err(anyhow!("Invalid piece type: {}", piece_type)),
        };

        let player_offset = if piece_type.is_ascii_uppercase() {
            0
        } else {
            8
        };
        let promotion_offset = if promoted { 16 } else { 0 };

        Ok(base_index + player_offset + promotion_offset)
    }

    /// Hash the hands (captured pieces) part of SFEN
    fn hash_hands(&self, hands: &str) -> Result<u64> {
        if hands == "-" {
            return Ok(0); // No pieces in hand
        }

        let mut hash = 0u64;
        let mut chars = hands.chars().peekable();

        while let Some(ch) = chars.next() {
            let (count, piece_ch) = if ch.is_ascii_digit() {
                // Multiple pieces (e.g., "2P" for 2 pawns, "10P" for 10 pawns)
                let mut count_str = String::new();
                count_str.push(ch);

                // Collect all consecutive digits
                while let Some(&next_ch) = chars.peek() {
                    if next_ch.is_ascii_digit() {
                        count_str.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }

                let count: usize =
                    count_str.parse().map_err(|_| anyhow!("Invalid piece count: {}", count_str))?;

                if let Some(piece_ch) = chars.next() {
                    (count, piece_ch)
                } else {
                    return Err(anyhow!("Invalid hand format: digit without piece"));
                }
            } else {
                // Single piece (e.g., "P" for 1 pawn)
                (1, ch)
            };

            // Get hand piece index (0-13: 7 piece types * 2 players)
            let hand_index = self.get_hand_piece_index(piece_ch)?;

            // XOR hash for each piece in hand
            // Use different hash for different counts
            for i in 0..count {
                // Rotate the base hash to get different values for different counts
                let piece_hash = self.zobrist_hand[hand_index].rotate_left((i * 7) as u32);
                hash ^= piece_hash;
            }
        }

        Ok(hash)
    }

    /// Get hand piece index for Zobrist table
    fn get_hand_piece_index(&self, piece_type: char) -> Result<usize> {
        let base_index = match piece_type.to_ascii_uppercase() {
            'P' => 0, // Pawn
            'L' => 1, // Lance
            'N' => 2, // Knight
            'S' => 3, // Silver
            'G' => 4, // Gold
            'B' => 5, // Bishop
            'R' => 6, // Rook
            _ => return Err(anyhow!("Invalid hand piece type: {}", piece_type)),
        };

        let player_offset = if piece_type.is_ascii_uppercase() {
            0
        } else {
            7
        };
        Ok(base_index + player_offset)
    }

    /// Simple pseudo-random number generator for Zobrist table initialization
    fn next_random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
}

impl Default for PositionHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zobrist_random_generation() {
        let mut state = 0x123456789abcdef0u64;
        let val1 = PositionHasher::next_random(&mut state);
        let val2 = PositionHasher::next_random(&mut state);

        assert_ne!(val1, val2);
        assert_ne!(val1, 0);
        assert_ne!(val2, 0);
    }

    #[test]
    fn test_piece_index_calculation() {
        let hasher = PositionHasher::new();

        // Test basic pieces
        assert_eq!(hasher.get_piece_index('P', false).unwrap(), 0); // Black pawn
        assert_eq!(hasher.get_piece_index('p', false).unwrap(), 8); // White pawn
        assert_eq!(hasher.get_piece_index('K', false).unwrap(), 7); // Black king
        assert_eq!(hasher.get_piece_index('k', false).unwrap(), 15); // White king

        // Test promoted pieces
        assert_eq!(hasher.get_piece_index('P', true).unwrap(), 16); // Promoted black pawn
        assert_eq!(hasher.get_piece_index('p', true).unwrap(), 24); // Promoted white pawn
    }

    #[test]
    fn test_different_turn_different_hash() {
        let hasher = PositionHasher::new();

        // Same position, different turn
        let sfen_black = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let sfen_white = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1";

        let hash_black = hasher.hash_sfen_position(sfen_black).unwrap();
        let hash_white = hasher.hash_sfen_position(sfen_white).unwrap();

        assert_ne!(hash_black, hash_white, "Different turn should produce different hash");
    }

    #[test]
    fn test_different_hands_different_hash() {
        let hasher = PositionHasher::new();

        // Same position and turn, different hands
        let sfen_no_hands = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let sfen_with_pawn = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b P 1";
        let sfen_with_2pawns = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 2P 1";

        let hash_no_hands = hasher.hash_sfen_position(sfen_no_hands).unwrap();
        let hash_with_pawn = hasher.hash_sfen_position(sfen_with_pawn).unwrap();
        let hash_with_2pawns = hasher.hash_sfen_position(sfen_with_2pawns).unwrap();

        assert_ne!(hash_no_hands, hash_with_pawn, "Different hands should produce different hash");
        assert_ne!(
            hash_with_pawn, hash_with_2pawns,
            "Different number of pieces should produce different hash"
        );
        assert_ne!(hash_no_hands, hash_with_2pawns, "No hands vs 2 pawns should be different");
    }

    #[test]
    fn test_after_move_different_hash() {
        let hasher = PositionHasher::new();

        // Initial position
        let sfen_initial = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        // After 7g7f
        let sfen_after_76 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL w - 2";

        let hash_initial = hasher.hash_sfen_position(sfen_initial).unwrap();
        let hash_after = hasher.hash_sfen_position(sfen_after_76).unwrap();

        assert_ne!(hash_initial, hash_after, "Position after 76歩 should have different hash");
    }

    #[test]
    fn test_multi_digit_hands() {
        let hasher = PositionHasher::new();

        // Test various hand combinations including multi-digit counts
        let sfen_10pawns = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 10P 1";
        let sfen_mixed = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 10P2n3s 1";
        let sfen_18pawns = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 18p 1";

        // These should all parse without error
        let hash1 = hasher.hash_sfen_position(sfen_10pawns).unwrap();
        let hash2 = hasher.hash_sfen_position(sfen_mixed).unwrap();
        let hash3 = hasher.hash_sfen_position(sfen_18pawns).unwrap();

        // All should be different
        assert_ne!(hash1, hash2);
        assert_ne!(hash2, hash3);
        assert_ne!(hash1, hash3);
    }
}
