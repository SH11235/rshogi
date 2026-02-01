//! 型エイリアスの集約（追加時はここだけ更新）
//!
//! 新しいアーキテクチャ追加時に、型エイリアスをここに追加するだけで
//! prelude.rs 経由で halfka/*.rs や halfkp/*.rs から利用可能になる。

// HalfKA_hm 型エイリアス
pub use crate::nnue::network_halfka_hm::{
    // L1=1024, L2=8, L3=96
    HalfKA_hm1024CReLU,
    // L1=1024, L2=8, L3=32
    HalfKA_hm1024_8_32CReLU,
    // L1=256, L2=32, L3=32
    HalfKA_hm256CReLU,
    // L1=512, L2=8, L3=96
    HalfKA_hm512CReLU,
    // L1=512, L2=32, L3=32
    HalfKA_hm512_32_32CReLU,
};

// HalfKA 型エイリアス
pub use crate::nnue::network_halfka::{
    // L1=1024, L2=8, L3=96
    HalfKA1024CReLU,
    // L1=1024, L2=8, L3=32
    HalfKA1024_8_32CReLU,
    // L1=256, L2=32, L3=32
    HalfKA256CReLU,
    // L1=512, L2=8, L3=96
    HalfKA512CReLU,
    // L1=512, L2=32, L3=32
    HalfKA512_32_32CReLU,
};

// HalfKP 型エイリアス
pub use crate::nnue::network_halfkp::{
    // L1=1024, L2=8, L3=32
    HalfKP1024_8_32CReLU,
    // L1=256, L2=32, L3=32
    HalfKP256CReLU,
    // L1=512, L2=8, L3=96
    HalfKP512CReLU,
    // L1=512, L2=32, L3=32
    HalfKP512_32_32CReLU,
    // L1=512, L2=8, L3=64
    HalfKP512_8_64CReLU,
    // L1=768, L2=16, L3=64
    HalfKP768CReLU,
};
