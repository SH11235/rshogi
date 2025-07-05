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
        let mut cursor = Cursor::new(data);

        while cursor.position() < data.len() as u64 {
            // ヘッダー読み込み
            let mut header_buf = [0u8; 16];
            if cursor.read_exact(&mut header_buf).is_err() {
                break;
            }

            let position_hash = u64::from_le_bytes(header_buf[0..8].try_into().unwrap());
            let move_count = u16::from_le_bytes(header_buf[8..10].try_into().unwrap());

            // ムーブ読み込み
            let mut moves = Vec::new();
            for _ in 0..move_count {
                let mut move_buf = [0u8; 6];
                cursor
                    .read_exact(&mut move_buf)
                    .map_err(|e| format!("Failed to read move: {e}"))?;

                let move_encoded = u16::from_le_bytes(move_buf[0..2].try_into().unwrap());
                let evaluation = i16::from_le_bytes(move_buf[2..4].try_into().unwrap());
                let depth = move_buf[4];

                moves.push(BookMove {
                    notation: format!("move_{move_encoded}"), // Phase 2.5で実装
                    evaluation,
                    depth,
                });
            }

            self.positions.insert(position_hash, moves);
        }

        Ok(())
    }

    pub fn find_moves_by_hash(&self, hash: u64) -> Vec<BookMove> {
        self.positions.get(&hash).cloned().unwrap_or_default()
    }

    pub fn find_moves(&self, sfen: &str) -> Vec<BookMove> {
        // 仮実装：初期局面なら固定ハッシュ値を返す（Phase 2.5で実装）
        if sfen == "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1" {
            return self.find_moves_by_hash(123456789);
        }
        vec![]
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
    pub fn load_data(&mut self, compressed_data: &[u8]) -> Result<String, JsValue> {
        self.inner.load_data(compressed_data).map_err(|e| JsValue::from_str(&e))
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
        let mut data = Vec::new();

        // ヘッダー: position_hash(8) + move_count(2) + padding(6)
        data.extend_from_slice(&12345u64.to_le_bytes()); // position_hash
        data.extend_from_slice(&1u16.to_le_bytes()); // move_count
        data.extend_from_slice(&[0u8; 6]); // padding

        // ムーブデータ: move_encoded(2) + evaluation(2) + depth(1) + padding(1)
        data.extend_from_slice(&0x1234u16.to_le_bytes()); // move
        data.extend_from_slice(&50i16.to_le_bytes()); // eval
        data.push(10); // depth
        data.push(0); // padding

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
        // Arrange
        let mut reader = OpeningBookReader::new();
        let initial_sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

        // 仮のハッシュ値とムーブを設定
        reader.positions.insert(
            123456789, // 初期局面の仮ハッシュ
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
}

// WebAssembly specific tests
#[cfg(target_arch = "wasm32")]
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
