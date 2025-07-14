// opening_book_reader.rs - WebAssembly用の定跡読み込みモジュール

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Cursor, Read};
use wasm_bindgen::prelude::*;

pub struct OpeningBookReader {
    positions: HashMap<u64, Vec<BookMove>>,
    loaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookMove {
    pub notation: String,
    pub evaluation: i16,
    pub depth: u8,
}

impl Default for OpeningBookReader {
    fn default() -> Self {
        Self::new()
    }
}

impl OpeningBookReader {
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
            loaded: false,
        }
    }

    pub fn position_count(&self) -> usize {
        self.positions.len()
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn load_data(&mut self, compressed_data: &[u8]) -> Result<String, String> {
        if compressed_data.is_empty() {
            self.loaded = true;
            return Ok("Loaded 0 positions".to_string());
        }

        // 圧縮データを解凍
        let decompressed = self
            .decompress_data(compressed_data)
            .map_err(|e| format!("Failed to decompress: {e}"))?;

        // バイナリデータをパース
        self.parse_binary_data(&decompressed)?;

        self.loaded = true;
        Ok(format!("Loaded {} positions", self.positions.len()))
    }

    fn decompress_data(&self, compressed: &[u8]) -> Result<Vec<u8>, io::Error> {
        let mut decoder = GzDecoder::new(compressed);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        Ok(decompressed)
    }

    fn parse_binary_data(&mut self, data: &[u8]) -> Result<(), String> {
        use crate::opening_book::MoveEncoder;

        let mut cursor = Cursor::new(data);

        // ファイルヘッダーを読み込み（16バイト）
        if data.len() >= 16 {
            let mut file_header = [0u8; 16];
            cursor
                .read_exact(&mut file_header)
                .map_err(|e| format!("Failed to read file header: {e}"))?;

            // マジックバイトの確認
            if &file_header[0..4] == b"SFEN" {
                // ファイルヘッダーが存在する場合はスキップ済み
                let version = u32::from_le_bytes(file_header[4..8].try_into().unwrap());
                let position_count = u32::from_le_bytes(file_header[8..12].try_into().unwrap());
                // ファイルヘッダー情報（必要に応じてログ出力）
                println!("Found SFEN header: version={version}, position_count={position_count}");
            } else {
                // ファイルヘッダーがない場合は位置を戻す
                println!("No SFEN header found, parsing from beginning");
                cursor.set_position(0);
            }
        }

        let mut positions_read = 0;
        while cursor.position() < data.len() as u64 {
            // 位置ヘッダー読み込み
            let mut header_buf = [0u8; 16];
            if cursor.read_exact(&mut header_buf).is_err() {
                break;
            }

            let position_hash = u64::from_le_bytes(header_buf[0..8].try_into().unwrap());
            let _best_move = u16::from_le_bytes(header_buf[8..10].try_into().unwrap());
            let _evaluation = i16::from_le_bytes(header_buf[10..12].try_into().unwrap());
            let _depth = header_buf[12];
            let move_count = header_buf[13] as u16; // move_countは1バイト

            // デバッグ用（必要に応じてコメントアウト）
            if positions_read < 3 {
                println!(
                    "Position {}: hash={}, move_count={}, cursor_pos={}",
                    positions_read,
                    position_hash,
                    move_count,
                    cursor.position()
                );
            }

            // ムーブ読み込み
            let mut moves = Vec::new();
            for move_idx in 0..move_count {
                if cursor.position() + 6 > data.len() as u64 {
                    return Err(format!("Not enough data for move {} at position {}: need 6 bytes but only {} remaining", 
                                     move_idx, cursor.position(), data.len() as u64 - cursor.position()));
                }

                let mut move_buf = [0u8; 6];
                cursor.read_exact(&mut move_buf).map_err(|e| {
                    format!(
                        "Failed to read move {} at position {}: {}",
                        move_idx,
                        cursor.position(),
                        e
                    )
                })?;

                let move_encoded = u16::from_le_bytes(move_buf[0..2].try_into().unwrap());
                let evaluation = i16::from_le_bytes(move_buf[2..4].try_into().unwrap());
                let depth = move_buf[4];

                let move_notation = MoveEncoder::decode_move(move_encoded)
                    .unwrap_or_else(|_| format!("invalid_{move_encoded}"));

                moves.push(BookMove {
                    notation: move_notation,
                    evaluation,
                    depth,
                });
            }

            self.positions.insert(position_hash, moves);
            positions_read += 1;
        }

        println!("Successfully parsed {positions_read} positions");

        Ok(())
    }

    pub fn find_moves_by_hash(&self, hash: u64) -> Vec<BookMove> {
        self.positions.get(&hash).cloned().unwrap_or_default()
    }

    pub fn find_moves(&self, sfen: &str) -> Vec<BookMove> {
        use crate::opening_book::PositionHasher;

        match PositionHasher::hash_position(sfen) {
            Ok(hash) => self.find_moves_by_hash(hash),
            Err(_) => vec![],
        }
    }
}

// WebAssembly bindings
#[wasm_bindgen]
pub struct OpeningBookReaderWasm {
    inner: OpeningBookReader,
}

impl Default for OpeningBookReaderWasm {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl OpeningBookReaderWasm {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: OpeningBookReader::new(),
        }
    }

    #[wasm_bindgen]
    pub fn load_data(&mut self, compressed_data: Vec<u8>) -> Result<String, JsValue> {
        self.inner.load_data(&compressed_data).map_err(|e| JsValue::from_str(&e))
    }

    #[wasm_bindgen]
    pub fn find_moves(&self, sfen: &str) -> String {
        let moves = self.inner.find_moves(sfen);
        serde_json::to_string(&moves).unwrap_or_else(|_| "[]".to_string())
    }

    #[wasm_bindgen(getter)]
    pub fn position_count(&self) -> usize {
        self.inner.position_count()
    }

    #[wasm_bindgen(getter)]
    pub fn is_loaded(&self) -> bool {
        self.inner.is_loaded()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // テスト用ヘルパー関数
    /// 正しい形式の位置ヘッダーを作成（16バイト）
    fn create_position_header(
        position_hash: u64,
        best_move: u16,
        evaluation: i16,
        depth: u8,
        move_count: u8,
    ) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(&position_hash.to_le_bytes()); // 0-7: position_hash
        header.extend_from_slice(&best_move.to_le_bytes()); // 8-9: best_move
        header.extend_from_slice(&evaluation.to_le_bytes()); // 10-11: evaluation
        header.push(depth); // 12: depth
        header.push(move_count); // 13: move_count
        header.extend_from_slice(&[0u8; 2]); // 14-15: padding
        header
    }

    /// ムーブデータを作成（6バイト）
    fn create_move_data(move_encoded: u16, evaluation: i16, depth: u8) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&move_encoded.to_le_bytes()); // 0-1: move
        data.extend_from_slice(&evaluation.to_le_bytes()); // 2-3: evaluation
        data.push(depth); // 4: depth
        data.push(0); // 5: padding
        data
    }

    type TestMove = (u16, i16, u8);
    type TestPosition = (u64, Vec<TestMove>);

    /// テスト用の完全なバイナリデータを作成
    fn create_test_binary_data(
        positions: Vec<TestPosition>, // (hash, moves)
        with_file_header: bool,
    ) -> Vec<u8> {
        let mut data = Vec::new();

        // ファイルヘッダー（オプション）
        if with_file_header {
            data.extend_from_slice(b"SFEN"); // magic
            data.extend_from_slice(&1u32.to_le_bytes()); // version
            data.extend_from_slice(&(positions.len() as u32).to_le_bytes()); // position_count
            data.extend_from_slice(&0u32.to_le_bytes()); // checksum
        }

        // 各位置のデータ
        for (hash, moves) in positions {
            let move_count = moves.len() as u8;
            let best_move = if moves.is_empty() { 0 } else { moves[0].0 };
            let best_eval = if moves.is_empty() { 0 } else { moves[0].1 };
            let best_depth = if moves.is_empty() { 0 } else { moves[0].2 };

            // 位置ヘッダー
            data.extend(create_position_header(hash, best_move, best_eval, best_depth, move_count));

            // ムーブデータ
            for (move_encoded, eval, depth) in moves {
                data.extend(create_move_data(move_encoded, eval, depth));
            }
        }

        data
    }

    #[test]
    fn test_create_new_reader() {
        // Arrange & Act
        let reader = OpeningBookReader::new();

        // Assert
        assert_eq!(reader.position_count(), 0);
        assert!(!reader.is_loaded());
    }

    #[test]
    fn test_load_empty_data() {
        // Arrange
        let mut reader = OpeningBookReader::new();
        let empty_data = vec![];

        // Act
        let result = reader.load_data(&empty_data);

        // Assert
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Loaded 0 positions");
        assert!(reader.is_loaded());
    }

    #[test]
    fn test_load_invalid_data() {
        // Arrange
        let mut reader = OpeningBookReader::new();
        let invalid_data = vec![1, 2, 3]; // 無効なデータ

        // Act
        let result = reader.load_data(&invalid_data);

        // Assert
        assert!(result.is_err());
        assert!(!reader.is_loaded());
    }

    #[test]
    fn test_decompress_gzip_data() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Arrange
        let original_data = b"test data";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original_data).unwrap();
        let compressed = encoder.finish().unwrap();

        let reader = OpeningBookReader::new();

        // Act
        let result = reader.decompress_data(&compressed);

        // Assert
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), original_data);
    }

    #[test]
    fn test_read_single_position() {
        // Arrange: 1つの位置データを作成
        let data = create_test_binary_data(
            vec![(12345, vec![(0x1234, 50, 10)])], // 1つの位置に1つのムーブ
            false,                                 // ファイルヘッダーなし
        );

        let mut reader = OpeningBookReader::new();

        // Act
        let result = reader.parse_binary_data(&data);

        // Assert
        assert!(result.is_ok());
        assert_eq!(reader.position_count(), 1);

        let moves = reader.find_moves_by_hash(12345);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].evaluation, 50);
        assert_eq!(moves[0].depth, 10);
    }

    #[test]
    fn test_find_moves_by_sfen() {
        use crate::opening_book::PositionHasher;

        // Arrange
        let mut reader = OpeningBookReader::new();
        let initial_sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

        // 実際のハッシュ値を計算して使用
        let hash = PositionHasher::hash_position(initial_sfen).unwrap();
        reader.positions.insert(
            hash,
            vec![BookMove {
                notation: "7g7f".to_string(),
                evaluation: 50,
                depth: 10,
            }],
        );

        // Act
        let moves = reader.find_moves(initial_sfen);

        // Assert
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].notation, "7g7f");
    }

    #[test]
    fn test_find_moves_with_real_hash() {
        use crate::opening_book::PositionHasher;

        // Arrange
        let mut reader = OpeningBookReader::new();
        let initial_sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

        // 実際のハッシュ値を計算
        let real_hash = PositionHasher::hash_position(initial_sfen).unwrap();

        reader.positions.insert(
            real_hash,
            vec![BookMove {
                notation: "7g7f".to_string(),
                evaluation: 50,
                depth: 10,
            }],
        );

        // Act
        let moves = reader.find_moves(initial_sfen);

        // Assert
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].notation, "7g7f");
    }

    #[test]
    fn test_decode_real_moves() {
        use crate::opening_book::MoveEncoder;

        // Arrange
        let mut reader = OpeningBookReader::new();
        let move_7g7f = MoveEncoder::encode_move("7g7f").unwrap();

        // バイナリデータを作成
        let data = create_test_binary_data(
            vec![(12345, vec![(move_7g7f, 50, 10)])], // 7g7fのムーブ
            false,
        );

        // Act
        reader.parse_binary_data(&data).unwrap();

        // Assert
        let moves = reader.find_moves_by_hash(12345);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].notation, "7g7f");
        assert_eq!(moves[0].evaluation, 50);
        assert_eq!(moves[0].depth, 10);
    }

    #[test]
    fn test_load_data_with_file_header() {
        use crate::opening_book::MoveEncoder;

        // Arrange
        let mut reader = OpeningBookReader::new();
        let move_7g7f = MoveEncoder::encode_move("7g7f").unwrap();

        // ファイルヘッダー付きバイナリデータを作成
        let data = create_test_binary_data(
            vec![(12345, vec![(move_7g7f, 50, 10)])], // 7g7fのムーブ
            true,                                     // ファイルヘッダーあり
        );

        // Act
        let result = reader.parse_binary_data(&data);

        // Assert
        assert!(result.is_ok());
        assert_eq!(reader.position_count(), 1);
        let moves = reader.find_moves_by_hash(12345);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].notation, "7g7f");
        assert_eq!(moves[0].evaluation, 50);
        assert_eq!(moves[0].depth, 10);
    }

    #[test]
    fn test_initial_position_hash_calculation() {
        use crate::opening_book::PositionHasher;

        // 初期盤面のSFEN
        const INITIAL_SFEN: &str =
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 0";

        // ハッシュを計算
        let hash = PositionHasher::hash_position(INITIAL_SFEN).unwrap();

        // ハッシュが生成されることを確認
        assert_ne!(hash, 0, "Hash should not be zero");
    }

    #[test]
    fn test_hash_consistency_with_move_count() {
        use crate::opening_book::PositionHasher;

        // 同じ局面で手数が異なる場合のハッシュを比較
        let sfen_move0 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 0";
        let sfen_move1 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let sfen_move15 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 15";

        let hash0 = PositionHasher::hash_position(sfen_move0).unwrap();
        let hash1 = PositionHasher::hash_position(sfen_move1).unwrap();
        let hash15 = PositionHasher::hash_position(sfen_move15).unwrap();

        // 手数が異なっても同じハッシュになることを確認
        assert_eq!(hash0, hash1, "Hash should be the same regardless of move count");
        assert_eq!(hash0, hash15, "Hash should be the same regardless of move count");
    }

    #[test]
    fn test_different_positions_different_hashes() {
        use crate::opening_book::PositionHasher;

        // 初期盤面のSFEN（手数0）
        const INITIAL_SFEN: &str =
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 0";
        // 2六歩の後の盤面
        const AFTER_26_SFEN: &str =
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL b - 1";

        let hash_initial = PositionHasher::hash_position(INITIAL_SFEN).unwrap();
        let hash_after_26 = PositionHasher::hash_position(AFTER_26_SFEN).unwrap();

        // 異なる局面は異なるハッシュになることを確認
        assert_ne!(hash_initial, hash_after_26, "Different positions should have different hashes");
    }

    #[test]
    fn test_multiple_positions_with_moves() {
        use crate::opening_book::MoveEncoder;

        // Arrange
        let mut reader = OpeningBookReader::new();
        let move_7g7f = MoveEncoder::encode_move("7g7f").unwrap();
        let move_2g2f = MoveEncoder::encode_move("2g2f").unwrap();
        let move_8h7g = MoveEncoder::encode_move("8h7g").unwrap();

        // 複数の位置データを作成
        let data = create_test_binary_data(
            vec![
                (12345, vec![(move_7g7f, 50, 10), (move_2g2f, 45, 9)]), // 位置1: 2つのムーブ
                (67890, vec![(move_8h7g, 30, 8)]),                      // 位置2: 1つのムーブ
                (11111, vec![]),                                        // 位置3: ムーブなし
            ],
            false,
        );

        // Act
        reader.parse_binary_data(&data).unwrap();

        // Assert
        assert_eq!(reader.position_count(), 3);

        // 位置1のムーブを確認
        let moves1 = reader.find_moves_by_hash(12345);
        assert_eq!(moves1.len(), 2);
        assert_eq!(moves1[0].notation, "7g7f");
        assert_eq!(moves1[1].notation, "2g2f");

        // 位置2のムーブを確認
        let moves2 = reader.find_moves_by_hash(67890);
        assert_eq!(moves2.len(), 1);
        assert_eq!(moves2[0].notation, "8h7g");

        // 位置3のムーブを確認（空）
        let moves3 = reader.find_moves_by_hash(11111);
        assert_eq!(moves3.len(), 0);
    }

    #[test]
    #[ignore] // 大容量ファイルを使うため通常のテストではスキップ
    fn test_load_real_opening_book_file() {
        use std::fs::File;
        use std::io::Read;

        // テストファイルのパスを調整（CI環境では使用できない場合があるため）
        let file_path = "converted_openings/opening_book_tournament.bin.gz";

        if let Ok(mut file) = File::open(file_path) {
            let mut data = Vec::new();
            if file.read_to_end(&mut data).is_ok() {
                let mut reader = OpeningBookReader::new();

                // Act
                println!("Loading file with {} bytes", data.len());
                let result = reader.load_data(&data);

                // Assert
                assert!(result.is_ok(), "Failed to load real opening book file: {result:?}");
                println!("Load result: {result:?}");
                println!("Position count: {}", reader.position_count());
                assert!(reader.position_count() > 0, "No positions loaded from file");

                // 初期局面での手があることを確認
                let initial_sfen =
                    "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
                let moves = reader.find_moves(initial_sfen);
                // 定跡ファイルによっては初期局面の手がない場合もあるため、エラーチェックのみ
                println!("Initial position moves found: {}", moves.len());
            }
        } else {
            println!("Real opening book file not found, skipping test");
        }
    }
}

// WebAssembly specific tests
#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn test_wasm_constructor() {
        let reader = OpeningBookReaderWasm::new();
        assert_eq!(reader.position_count(), 0);
        assert!(!reader.is_loaded());
    }

    #[wasm_bindgen_test]
    fn test_wasm_find_moves() {
        let reader = OpeningBookReaderWasm::new();
        let moves_json = reader.find_moves("test");
        assert_eq!(moves_json, "[]");
    }
}
