//! Move encoder for converting Shogi move notation to/from 16-bit integers
//!
//! This module provides efficient encoding of Shogi moves into 16-bit integers
//! for compact storage in the binary opening book format.
//!
//! Encoding format:
//! - Bits 15-14: Move type (00=normal, 01=promotion, 10=drop, 11=reserved)
//! - Bits 13-7:  From square (7 bits) OR piece type for drops (7 bits)
//! - Bits 6-0:   To square (7 bits)
//!
//! Square encoding: file * 9 + rank - 10 (0-80 for squares 1a-9i)

use anyhow::{Result, anyhow};

/// Move encoder for converting between move notation and 16-bit integers
pub struct MoveEncoder;

impl MoveEncoder {
    /// Encode a move notation string to a 16-bit integer
    /// 
    /// Supports:
    /// - Normal moves: "7g7f", "8i7g"
    /// - Promotion moves: "3d3c+", "2e2d+"
    /// - Drop moves: "P*5f", "N*4e"
    pub fn encode_move(move_notation: &str) -> Result<u16> {
        if move_notation.is_empty() {
            return Err(anyhow!("Empty move notation"));
        }

        // Check for drop moves (contain '*')
        if move_notation.contains('*') {
            Self::encode_drop_move(move_notation)
        } else {
            // Normal or promotion move
            Self::encode_normal_move(move_notation)
        }
    }

    /// Decode a 16-bit integer back to move notation
    pub fn decode_move(encoded: u16) -> Result<String> {
        // Extract move type from top 2 bits
        let move_type = (encoded >> 14) & 0x3;

        match move_type {
            0 => Self::decode_normal_move(encoded, false),  // Normal move
            1 => Self::decode_normal_move(encoded, true),   // Promotion move
            2 => Self::decode_drop_move(encoded),           // Drop move
            _ => Err(anyhow!("Reserved move type: {}", move_type)),
        }
    }

    /// Encode a normal move (with optional promotion)
    fn encode_normal_move(move_notation: &str) -> Result<u16> {
        let promotion = move_notation.ends_with('+');
        let move_part = if promotion {
            &move_notation[..move_notation.len() - 1]
        } else {
            move_notation
        };

        if move_part.len() != 4 {
            return Err(anyhow!("Invalid normal move format: {}", move_notation));
        }

        // Parse squares: "7g7f" -> from=7g, to=7f
        let from_file = move_part.chars().nth(0).unwrap()
            .to_digit(10).ok_or_else(|| anyhow!("Invalid from file: {}", move_part))? as u8;
        let from_rank = move_part.chars().nth(1).unwrap()
            .to_ascii_lowercase() as u8;
        let to_file = move_part.chars().nth(2).unwrap()
            .to_digit(10).ok_or_else(|| anyhow!("Invalid to file: {}", move_part))? as u8;
        let to_rank = move_part.chars().nth(3).unwrap()
            .to_ascii_lowercase() as u8;

        // Validate ranges
        if !(1..=9).contains(&from_file) || !(1..=9).contains(&to_file) {
            return Err(anyhow!("Invalid file range: {}", move_notation));
        }
        if from_rank < b'a' || from_rank > b'i' || to_rank < b'a' || to_rank > b'i' {
            return Err(anyhow!("Invalid rank range: {}", move_notation));
        }

        // Convert to square indices (0-80)
        let from_square = ((from_file - 1) * 9 + (from_rank - b'a')) as u16;
        let to_square = ((to_file - 1) * 9 + (to_rank - b'a')) as u16;

        // Build encoding: [type:2][from:7][to:7]
        let move_type = if promotion { 1u16 } else { 0u16 };
        let encoded = (move_type << 14) | (from_square << 7) | to_square;

        Ok(encoded)
    }

    /// Encode a drop move
    fn encode_drop_move(move_notation: &str) -> Result<u16> {
        let parts: Vec<&str> = move_notation.split('*').collect();
        if parts.len() != 2 {
            return Err(anyhow!("Invalid drop notation: {}", move_notation));
        }

        let piece = Self::encode_piece(parts[0])?;
        let square = Self::encode_square(parts[1])?;

        // Build encoding: [type:2][piece:7][square:7]
        let encoded = (2u16 << 14) | ((piece as u16) << 7) | (square as u16);

        Ok(encoded)
    }

    /// Decode a normal move
    fn decode_normal_move(encoded: u16, promotion: bool) -> Result<String> {
        // Extract from and to squares
        let from_square = ((encoded >> 7) & 0x7F) as u8;
        let to_square = (encoded & 0x7F) as u8;

        if from_square > 80 || to_square > 80 {
            return Err(anyhow!("Invalid square encoding: from={}, to={}", from_square, to_square));
        }

        // Convert back to file/rank notation
        let from_file = (from_square / 9) + 1;
        let from_rank = (from_square % 9) + b'a';
        let to_file = (to_square / 9) + 1;
        let to_rank = (to_square % 9) + b'a';

        let mut result = format!("{}{}{}{}", 
            from_file, from_rank as char, to_file, to_rank as char);

        if promotion {
            result.push('+');
        }

        Ok(result)
    }

    /// Decode a drop move
    fn decode_drop_move(encoded: u16) -> Result<String> {
        // Extract piece and square
        let piece_code = ((encoded >> 7) & 0x7F) as u8;
        let square = (encoded & 0x7F) as u8;

        if square > 80 {
            return Err(anyhow!("Invalid square encoding: {}", square));
        }

        let piece = Self::decode_piece(piece_code)?;
        let square_notation = Self::decode_square(square)?;

        Ok(format!("{}*{}", piece, square_notation))
    }

    /// Encode piece type to number
    fn encode_piece(piece: &str) -> Result<u8> {
        match piece {
            "P" => Ok(0),  // Pawn
            "L" => Ok(1),  // Lance
            "N" => Ok(2),  // Knight
            "S" => Ok(3),  // Silver
            "G" => Ok(4),  // Gold
            "B" => Ok(5),  // Bishop
            "R" => Ok(6),  // Rook
            _ => Err(anyhow!("Invalid piece type: {}", piece)),
        }
    }

    /// Decode piece number to type
    fn decode_piece(piece_code: u8) -> Result<&'static str> {
        match piece_code {
            0 => Ok("P"),
            1 => Ok("L"),
            2 => Ok("N"),
            3 => Ok("S"),
            4 => Ok("G"),
            5 => Ok("B"),
            6 => Ok("R"),
            _ => Err(anyhow!("Invalid piece code: {}", piece_code)),
        }
    }

    /// Encode square notation to number
    fn encode_square(square: &str) -> Result<u8> {
        if square.len() != 2 {
            return Err(anyhow!("Invalid square notation: {}", square));
        }

        let file = square.chars().nth(0).unwrap()
            .to_digit(10).ok_or_else(|| anyhow!("Invalid file: {}", square))? as u8;
        let rank = square.chars().nth(1).unwrap().to_ascii_lowercase() as u8;

        if !(1..=9).contains(&file) {
            return Err(anyhow!("Invalid file range: {}", square));
        }
        if rank < b'a' || rank > b'i' {
            return Err(anyhow!("Invalid rank range: {}", square));
        }

        Ok((file - 1) * 9 + (rank - b'a'))
    }

    /// Decode square number to notation
    fn decode_square(square: u8) -> Result<String> {
        if square > 80 {
            return Err(anyhow!("Invalid square number: {}", square));
        }

        let file = (square / 9) + 1;
        let rank = (square % 9) + b'a';

        Ok(format!("{}{}", file, rank as char))
    }
}