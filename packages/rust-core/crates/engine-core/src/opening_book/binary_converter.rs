//! Binary converter for opening book data
//!
//! This module converts SFEN opening book data to a compact binary format,
//! reducing file size by 70-90% while maintaining fast lookup performance.

use crate::opening_book::{
    CompactMove, CompactPosition, MoveEncoder, PositionFilter, PositionHasher, RawMove,
    RawSfenEntry,
};
use anyhow::{anyhow, Result};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use std::io::{Read, Write};

/// File header for binary opening book format
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BinaryFileHeader {
    pub magic: [u8; 4],
    pub version: u32,
    pub position_count: u32,
    pub checksum: u32,
}

/// Statistics about the conversion process
#[derive(Debug, Clone)]
pub struct ConversionStats {
    pub positions_written: usize,
    pub total_moves: usize,
    pub bytes_written: usize,
    pub compression_ratio: f64,
}

/// Binary entry containing position header and moves
#[derive(Debug, Clone)]
pub struct BinaryEntry {
    pub header: CompactPosition,
    pub moves: Vec<CompactMove>,
}

/// Binary converter for opening book data
pub struct BinaryConverter {}

impl BinaryConverter {
    /// Create a new binary converter
    pub fn new() -> Self {
        Self {}
    }

    /// Convert a single SFEN entry to binary format
    pub fn convert_entry(&self, entry: &RawSfenEntry) -> Result<BinaryEntry> {
        // Hash the position - includes board, turn and hands (but NOT move count)
        // 手数は定跡検索では使用しないため、ハッシュ生成時に含めない
        let position_str = format!("{} {} {}", entry.position, entry.turn, entry.hand);
        let position_hash = PositionHasher::hash_position(&position_str)?;

        // Find best move
        let best_move = entry
            .moves
            .iter()
            .max_by_key(|m| m.evaluation)
            .ok_or_else(|| anyhow!("No moves in entry"))?;

        // Encode best move
        let best_move_encoded = MoveEncoder::encode_move(&best_move.move_notation)?;

        // Create position header
        let header = CompactPosition {
            position_hash,
            best_move: best_move_encoded,
            evaluation: best_move.evaluation as i16,
            depth: best_move.depth as u8,
            move_count: entry.moves.len() as u8,
            popularity: 1, // Default popularity
            reserved: 0,
        };

        // Convert moves
        let moves = entry.moves.iter().map(|m| self.convert_move(m)).collect::<Result<Vec<_>>>()?;

        Ok(BinaryEntry { header, moves })
    }

    /// Convert a raw move to compact format
    fn convert_move(&self, raw_move: &RawMove) -> Result<CompactMove> {
        let move_encoded = MoveEncoder::encode_move(&raw_move.move_notation)?;

        Ok(CompactMove {
            move_encoded,
            evaluation: raw_move.evaluation as i16,
            depth: raw_move.depth as u8,
            reserved: 0,
        })
    }

    /// Convert multiple entries
    pub fn convert_entries(&self, entries: &[RawSfenEntry]) -> Result<Vec<BinaryEntry>> {
        entries.iter().map(|e| self.convert_entry(e)).collect()
    }

    /// Filter and convert entries
    pub fn filter_and_convert(
        &self,
        entries: &mut [RawSfenEntry],
        filter: &PositionFilter,
    ) -> Result<Vec<BinaryEntry>> {
        let mut filtered_entries = Vec::new();

        for entry in entries.iter_mut() {
            if filter.filter_entry(entry) {
                filtered_entries.push(self.convert_entry(entry)?);
            }
        }

        Ok(filtered_entries)
    }

    /// Encode position header to bytes
    pub fn encode_position_header(header: &CompactPosition) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16);

        bytes.extend_from_slice(&header.position_hash.to_le_bytes());
        bytes.extend_from_slice(&header.best_move.to_le_bytes());
        bytes.extend_from_slice(&header.evaluation.to_le_bytes());
        bytes.push(header.depth);
        bytes.push(header.move_count);
        bytes.push(header.popularity);
        bytes.push(header.reserved);

        bytes
    }

    /// Decode position header from bytes
    pub fn decode_position_header(bytes: &[u8]) -> Result<CompactPosition> {
        if bytes.len() < 16 {
            return Err(anyhow!("Invalid position header size"));
        }

        Ok(CompactPosition {
            position_hash: u64::from_le_bytes(bytes[0..8].try_into()?),
            best_move: u16::from_le_bytes(bytes[8..10].try_into()?),
            evaluation: i16::from_le_bytes(bytes[10..12].try_into()?),
            depth: bytes[12],
            move_count: bytes[13],
            popularity: bytes[14],
            reserved: bytes[15],
        })
    }

    /// Encode move to bytes
    pub fn encode_move(move_data: &CompactMove) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(6);

        bytes.extend_from_slice(&move_data.move_encoded.to_le_bytes());
        bytes.extend_from_slice(&move_data.evaluation.to_le_bytes());
        bytes.push(move_data.depth);
        bytes.push(move_data.reserved);

        bytes
    }

    /// Decode move from bytes
    pub fn decode_move(bytes: &[u8]) -> Result<CompactMove> {
        if bytes.len() < 6 {
            return Err(anyhow!("Invalid move size"));
        }

        Ok(CompactMove {
            move_encoded: u16::from_le_bytes(bytes[0..2].try_into()?),
            evaluation: i16::from_le_bytes(bytes[2..4].try_into()?),
            depth: bytes[4],
            reserved: bytes[5],
        })
    }

    /// Encode file header
    pub fn encode_file_header(&self, header: &BinaryFileHeader) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16);

        bytes.extend_from_slice(&header.magic);
        bytes.extend_from_slice(&header.version.to_le_bytes());
        bytes.extend_from_slice(&header.position_count.to_le_bytes());
        bytes.extend_from_slice(&header.checksum.to_le_bytes());

        bytes
    }

    /// Decode file header
    pub fn decode_file_header(&self, bytes: &[u8]) -> Result<BinaryFileHeader> {
        if bytes.len() < 16 {
            return Err(anyhow!("Invalid file header size"));
        }

        Ok(BinaryFileHeader {
            magic: bytes[0..4].try_into()?,
            version: u32::from_le_bytes(bytes[4..8].try_into()?),
            position_count: u32::from_le_bytes(bytes[8..12].try_into()?),
            checksum: u32::from_le_bytes(bytes[12..16].try_into()?),
        })
    }

    /// Write binary data to writer
    pub fn write_binary<W: Write>(
        &self,
        entries: &[RawSfenEntry],
        writer: &mut W,
    ) -> Result<ConversionStats> {
        let binary_entries: Vec<BinaryEntry> =
            entries.iter().map(|e| self.convert_entry(e)).collect::<Result<Vec<_>>>()?;

        let mut data = Vec::new();
        let mut total_moves = 0;

        // Write positions and moves
        for entry in &binary_entries {
            data.extend(Self::encode_position_header(&entry.header));
            total_moves += entry.moves.len();

            for mov in &entry.moves {
                data.extend(Self::encode_move(mov));
            }
        }

        // Calculate checksum
        let checksum = self.calculate_checksum(&data);

        // Create and write header
        let header = BinaryFileHeader {
            magic: *b"SFEN",
            version: 1,
            position_count: binary_entries.len() as u32,
            checksum,
        };

        writer.write_all(&self.encode_file_header(&header))?;
        writer.write_all(&data)?;

        let bytes_written = 16 + data.len(); // header + data

        Ok(ConversionStats {
            positions_written: binary_entries.len(),
            total_moves,
            bytes_written,
            compression_ratio: 1.0, // No compression in basic write
        })
    }

    /// Read binary data from reader
    pub fn read_binary<R: Read>(&self, reader: &mut R) -> Result<Vec<BinaryEntry>> {
        // Read header
        let mut header_bytes = [0u8; 16];
        reader.read_exact(&mut header_bytes)?;
        let header = self.decode_file_header(&header_bytes)?;

        if &header.magic != b"SFEN" {
            return Err(anyhow!("Invalid file magic"));
        }

        // Read data
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;

        // Verify checksum
        let calculated_checksum = self.calculate_checksum(&data);
        if calculated_checksum != header.checksum {
            return Err(anyhow!("Checksum mismatch"));
        }

        // Parse entries
        let mut entries = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            // Read position header
            if offset + 16 > data.len() {
                break;
            }
            let pos_header = Self::decode_position_header(&data[offset..offset + 16])?;
            offset += 16;

            // Read moves
            let mut moves = Vec::new();
            for _ in 0..pos_header.move_count {
                if offset + 6 > data.len() {
                    return Err(anyhow!("Unexpected end of data"));
                }
                let mov = Self::decode_move(&data[offset..offset + 6])?;
                moves.push(mov);
                offset += 6;
            }

            entries.push(BinaryEntry {
                header: pos_header,
                moves,
            });
        }

        Ok(entries)
    }

    /// Compress data using gzip
    pub fn compress_data(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        Ok(encoder.finish()?)
    }

    /// Decompress data using gzip
    pub fn decompress_data(&self, compressed: &[u8]) -> Result<Vec<u8>> {
        let mut decoder = GzDecoder::new(compressed);
        let mut data = Vec::new();
        decoder.read_to_end(&mut data)?;
        Ok(data)
    }

    /// Calculate CRC32 checksum
    fn calculate_checksum(&self, data: &[u8]) -> u32 {
        // Simple checksum implementation (in production, use crc32 crate)
        let mut checksum = 0u32;
        for chunk in data.chunks(4) {
            let mut value = 0u32;
            for (i, &byte) in chunk.iter().enumerate() {
                value |= (byte as u32) << (i * 8);
            }
            checksum = checksum.wrapping_add(value);
        }
        checksum
    }
}

impl Default for BinaryConverter {
    fn default() -> Self {
        Self::new()
    }
}
