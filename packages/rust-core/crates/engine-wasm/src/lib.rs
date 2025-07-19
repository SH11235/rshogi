use wasm_bindgen::prelude::*;
use engine_core::{Engine, OpeningBookReader};

/// エンジンを保持するハンドル
#[wasm_bindgen]
pub struct WasmEngine {
    inner: Engine,
}

// TODO: Web向けのインターフェースを設計・実装
#[wasm_bindgen]
impl WasmEngine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmEngine {
        WasmEngine { inner: Engine::new(Default::default()) }
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
        self.inner.load_data(&compressed_data).map_err(|e: &str| JsValue::from_str(e))
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
