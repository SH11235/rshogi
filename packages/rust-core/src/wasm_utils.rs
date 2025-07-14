//! WebAssembly utilities shared across the crate
//!
//! This module provides common utilities for WASM targets,
//! including console logging and other browser-specific functionality.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::wasm_bindgen;

// Single extern block for console logging
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

/// Macro for logging to the browser console
#[macro_export]
macro_rules! console_log {
    ($($t:tt)*) => {
        #[cfg(target_arch = "wasm32")]
        {
            use $crate::wasm_utils::__console_log_impl;
            __console_log_impl(&format_args!($($t)*).to_string());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            eprintln!($($t)*);
        }
    }
}

// Internal function used by the macro
#[doc(hidden)]
#[cfg(target_arch = "wasm32")]
pub fn __console_log_impl(s: &str) {
    log(s);
}

// Re-export common WASM types
#[cfg(target_arch = "wasm32")]
pub use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_bindgen::JsValue;
