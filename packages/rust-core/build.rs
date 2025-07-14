//! Build script for shogi_core
//!
//! Detects available CPU features and sets appropriate cfg flags

use std::env;

fn main() {
    // Rebuild when build script or target metadata changes
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FEATURE");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();

    let has_avx2 = features.contains("avx2");
    let has_sse41 = features.contains("sse4.1");

    // Set cfg flags based on available features
    match arch.as_str() {
        "x86_64" => {
            println!("cargo:rustc-cfg=arch_x86_64");
            if has_avx2 {
                println!("cargo:rustc-cfg=compiletime_avx2");
            }
            if has_sse41 {
                println!("cargo:rustc-cfg=compiletime_sse41");
            }
        }
        "wasm32" => {
            println!("cargo:rustc-cfg=arch_wasm32");
        }
        _ => { /* 他はデフォルト */ }
    }

    // For SIMD intrinsics, we need to enable specific target features
    // This is done through RUSTFLAGS or target-specific configuration
}
