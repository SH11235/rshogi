//! 型エイリアスの集約（追加時はここだけ更新）
//!
//! 新しいアーキテクチャ追加時に、型エイリアスをここに追加するだけで
//! prelude.rs 経由で halfka/*.rs や halfkp/*.rs から利用可能になる。

// HalfKA 型エイリアス
pub use crate::nnue::network_halfka::{
    // L1=1024, L2=8, L3=96
    HalfKA1024CReLU,
    HalfKA1024Pairwise,
    HalfKA1024SCReLU,
    // L1=1024, L2=8, L3=32
    HalfKA1024_8_32CReLU,
    HalfKA1024_8_32SCReLU,
    // 新規追加はここに
    // L1=256
    HalfKA256CReLU,
    HalfKA256Pairwise,
    HalfKA256SCReLU,
    // L1=512, L2=8, L3=96
    HalfKA512CReLU,
    HalfKA512Pairwise,
    HalfKA512SCReLU,
};

// HalfKP 型エイリアス
pub use crate::nnue::network_halfkp::{
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
    // 新規追加はここに
};
