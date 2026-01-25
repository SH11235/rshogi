//! 型エイリアスの集約（追加時はここだけ更新）
//!
//! 新しいアーキテクチャ追加時に、型エイリアスをここに追加するだけで
//! prelude.rs 経由で halfka/*.rs や halfkp/*.rs から利用可能になる。

// HalfKA_hm 型エイリアス
pub use crate::nnue::network_halfka_hm::{
    // L1=1024, L2=8, L3=96
    HalfKA_hm1024CReLU,
    HalfKA_hm1024Pairwise,
    HalfKA_hm1024SCReLU,
    // L1=1024, L2=8, L3=32
    HalfKA_hm1024_8_32CReLU,
    HalfKA_hm1024_8_32Pairwise,
    HalfKA_hm1024_8_32SCReLU,
    // L1=256, L2=32, L3=32
    HalfKA_hm256CReLU,
    HalfKA_hm256Pairwise,
    HalfKA_hm256SCReLU,
    // L1=512, L2=8, L3=96
    HalfKA_hm512CReLU,
    HalfKA_hm512Pairwise,
    HalfKA_hm512SCReLU,
    // L1=512, L2=32, L3=32
    HalfKA_hm512_32_32CReLU,
    HalfKA_hm512_32_32Pairwise,
    HalfKA_hm512_32_32SCReLU,
};

// HalfKP 型エイリアス
pub use crate::nnue::network_halfkp::{
    // L1=1024, L2=8, L3=32
    HalfKP1024_8_32CReLU,
    HalfKP1024_8_32Pairwise,
    HalfKP1024_8_32SCReLU,
    // L1=256, L2=32, L3=32
    HalfKP256CReLU,
    HalfKP256Pairwise,
    HalfKP256SCReLU,
    // L1=512, L2=8, L3=96
    HalfKP512CReLU,
    HalfKP512Pairwise,
    HalfKP512SCReLU,
    // L1=512, L2=32, L3=32
    HalfKP512_32_32CReLU,
    HalfKP512_32_32Pairwise,
    HalfKP512_32_32SCReLU,
};
