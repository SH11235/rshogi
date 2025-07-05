//! SFEN parser for YaneuraOu opening book format
//!
//! This module provides functionality to parse YaneuraOu SFEN files line by line,
//! handling position definitions and move entries.

use crate::opening_book::{RawMove, RawSfenEntry};
use anyhow::{anyhow, Result};

/// Parser for YaneuraOu SFEN format files
pub struct SfenParser {
    /// Current position being parsed
    current_position: Option<RawSfenEntry>,
}

impl SfenParser {
    /// Create a new SFEN parser
    pub fn new() -> Self {
        Self {
            current_position: None,
        }
    }

    /// Parse a single line from the SFEN file
    ///
    /// Returns:
    /// - Ok(Some(entry)) when a complete position entry is finished
    /// - Ok(None) when parsing continues (header, position start, or move added)
    /// - Err(_) when the line is malformed
    pub fn parse_line(&mut self, line: &str) -> Result<Option<RawSfenEntry>> {
        let line = line.trim();

        // Skip empty lines, but if we have a current position, complete it
        if line.is_empty() {
            return Ok(self.current_position.take());
        }

        // Skip header lines
        if line.starts_with('#') {
            return Ok(None);
        }

        // Position lines start with "sfen"
        if line.starts_with("sfen ") {
            // If we have a current position, return it before starting a new one
            let mut result = None;
            if let Some(current) = self.current_position.take() {
                result = Some(current);
            }
            self.current_position = Some(self.parse_position_line(line)?);
            return Ok(result);
        }

        // Otherwise, it should be a move line
        if self.current_position.is_some() {
            let move_data = self.parse_move_line(line)?;
            self.current_position
                .as_mut()
                .unwrap()
                .moves
                .push(move_data);
            Ok(None)
        } else {
            Err(anyhow!("Move line without position: {}", line))
        }
    }

    /// Parse a position line (starts with "sfen")
    fn parse_position_line(&self, line: &str) -> Result<RawSfenEntry> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        if parts.len() < 5 {
            return Err(anyhow!("Invalid position line format: {}", line));
        }

        // Format: sfen <position> <turn> <hand> <move_count>
        if parts[0] != "sfen" {
            return Err(anyhow!("Position line must start with 'sfen': {}", line));
        }

        let position = parts[1].to_string();
        let turn = parts[2]
            .chars()
            .next()
            .ok_or_else(|| anyhow!("Invalid turn indicator: {}", parts[2]))?;
        let hand = parts[3].to_string();
        let move_count = parts[4]
            .parse::<u32>()
            .map_err(|_| anyhow!("Invalid move count: {}", parts[4]))?;

        Ok(RawSfenEntry {
            position,
            turn,
            hand,
            move_count,
            moves: Vec::new(),
        })
    }

    /// Parse a move line
    fn parse_move_line(&self, line: &str) -> Result<RawMove> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        if parts.len() < 5 {
            return Err(anyhow!("Invalid move line format: {}", line));
        }

        // Format: <move> <type> <eval> <depth> <nodes>
        let move_notation = parts[0].to_string();
        let move_type = parts[1].to_string();
        let evaluation = parts[2]
            .parse::<i32>()
            .map_err(|_| anyhow!("Invalid evaluation: {}", parts[2]))?;
        let depth = parts[3]
            .parse::<u32>()
            .map_err(|_| anyhow!("Invalid depth: {}", parts[3]))?;
        let nodes = parts[4]
            .parse::<u64>()
            .map_err(|_| anyhow!("Invalid nodes: {}", parts[4]))?;

        Ok(RawMove {
            move_notation,
            move_type,
            evaluation,
            depth,
            nodes,
        })
    }
}

impl Default for SfenParser {
    fn default() -> Self {
        Self::new()
    }
}
