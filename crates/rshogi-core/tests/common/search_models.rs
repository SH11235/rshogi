//! 探索と差分評価の回帰テスト用の決定的な非ゼロ重み。

use super::core;
use core::nnue::*;

pub fn for_each_model(mut check: impl FnMut(&str, Vec<u8>)) {
    if cfg!(any(feature = "halfkx-arch", feature = "nnue-runtime-dimensions")) {
        for_each_halfkx_model(&mut check);
    }
    if cfg!(any(
        feature = "nnue-runtime-dimensions",
        all(feature = "layerstacks-1536x16x32", feature = "ft-halfka_hm_merged")
    )) {
        let model = core::nnue::net_delta::test_utils::build_synthetic_layer_stacks(
            "HalfKaHmMerged",
            HALFKA_HM_DIMENSIONS,
            1536,
            16,
            32,
            4,
        );
        check("LayerStacks", model.bytes);
    }
}

pub fn for_each_halfkx_model(mut check: impl FnMut(&str, Vec<u8>)) {
    for (name, dimensions) in [
        ("HalfKP", HALFKP_DIMENSIONS),
        ("HalfKaSplit", HALFKA_DIMENSIONS),
        ("HalfKaMerged", HALFKA_MERGED_DIMENSIONS),
        ("HalfKaHmSplit", HALFKA_HM_SPLIT_DIMENSIONS),
        ("HalfKaHmMerged", HALFKA_HM_DIMENSIONS),
    ] {
        check(name, halfkx(name, dimensions));
    }
}

fn halfkx(name: &str, dimensions: usize) -> Vec<u8> {
    let l1 = 256;
    let arch = format!("Features={name}[{dimensions}->{l1}x2],l2=32,l3=32");
    let mut bytes = Vec::with_capacity(dimensions * l1 * 2 + 40000);
    for value in [NNUE_VERSION, 0, arch.len() as u32] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(arch.as_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for i in 0..l1 {
        bytes.extend_from_slice(&(32 + (i % 13) as i16).to_le_bytes());
    }
    for feature in 0..dimensions {
        for i in 0..l1 {
            let weight = ((feature * 17 + feature / 97 + i * 7) % 5) as i16 - 2;
            bytes.extend_from_slice(&weight.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for (input, output) in [(2 * l1, 32), (32, 32), (32, 1)] {
        for i in 0..output {
            bytes.extend_from_slice(&(64 + i as i32 * 3).to_le_bytes());
        }
        for out in 0..output {
            for inp in 0..input {
                bytes.push(if inp % 8 == out % 8 { 3 } else { 0 });
            }
        }
    }
    bytes
}
