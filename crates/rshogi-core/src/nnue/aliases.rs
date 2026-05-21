//! 型エイリアスの集約（追加時はここだけ更新）
//!
//! 新しいアーキテクチャ追加時に、型エイリアスをここに追加するだけで
//! prelude.rs 経由で halfka/*.rs や halfkp/*.rs から利用可能になる。

// HalfKA_hm 型エイリアス
pub use crate::nnue::network_halfka_hm::{
    // L1=256, L2=32, L3=32
    HalfKA_hm256CReLU,
    HalfKA_hm256Pairwise,
    HalfKA_hm256SCReLU,
    // L1=512, L2=8, L3=64
    HalfKA_hm512_8_64CReLU,
    HalfKA_hm512_8_64Pairwise,
    HalfKA_hm512_8_64SCReLU,
    // L1=512, L2=32, L3=32
    HalfKA_hm512_32_32CReLU,
    HalfKA_hm512_32_32Pairwise,
    HalfKA_hm512_32_32SCReLU,
    // L1=512, L2=8, L3=96
    HalfKA_hm512CReLU,
    HalfKA_hm512Pairwise,
    HalfKA_hm512SCReLU,
    // L1=768, L2=16, L3=64
    HalfKA_hm768CReLU,
    HalfKA_hm768Pairwise,
    HalfKA_hm768SCReLU,
    // L1=1024, L2=8, L3=32
    HalfKA_hm1024_8_32CReLU,
    HalfKA_hm1024_8_32Pairwise,
    HalfKA_hm1024_8_32SCReLU,
    // L1=1024, L2=8, L3=64
    HalfKA_hm1024_8_64CReLU,
    HalfKA_hm1024_8_64Pairwise,
    HalfKA_hm1024_8_64SCReLU,
    // L1=1024, L2=8, L3=96
    HalfKA_hm1024CReLU,
    HalfKA_hm1024Pairwise,
    HalfKA_hm1024SCReLU,
};

// HalfKA 型エイリアス
pub use crate::nnue::network_halfka::{
    // L1=256, L2=32, L3=32
    HalfKA256CReLU,
    HalfKA256Pairwise,
    HalfKA256SCReLU,
    // L1=512, L2=8, L3=64
    HalfKA512_8_64CReLU,
    HalfKA512_8_64Pairwise,
    HalfKA512_8_64SCReLU,
    // L1=512, L2=32, L3=32
    HalfKA512_32_32CReLU,
    HalfKA512_32_32Pairwise,
    HalfKA512_32_32SCReLU,
    // L1=512, L2=8, L3=96
    HalfKA512CReLU,
    HalfKA512Pairwise,
    HalfKA512SCReLU,
    // L1=768, L2=16, L3=64
    HalfKA768CReLU,
    HalfKA768Pairwise,
    HalfKA768SCReLU,
    // L1=1024, L2=8, L3=32
    HalfKA1024_8_32CReLU,
    HalfKA1024_8_32Pairwise,
    HalfKA1024_8_32SCReLU,
    // L1=1024, L2=8, L3=64
    HalfKA1024_8_64CReLU,
    HalfKA1024_8_64Pairwise,
    HalfKA1024_8_64SCReLU,
    // L1=1024, L2=8, L3=96
    HalfKA1024CReLU,
    HalfKA1024Pairwise,
    HalfKA1024SCReLU,
};

// HalfKaMerged 型エイリアス
pub use crate::nnue::network_halfka_merged::{
    // L1=256, L2=32, L3=32
    HalfKaMerged256CReLU,
    HalfKaMerged256Pairwise,
    HalfKaMerged256SCReLU,
    // L1=512, L2=8, L3=64
    HalfKaMerged512_8_64CReLU,
    HalfKaMerged512_8_64Pairwise,
    HalfKaMerged512_8_64SCReLU,
    // L1=512, L2=32, L3=32
    HalfKaMerged512_32_32CReLU,
    HalfKaMerged512_32_32Pairwise,
    HalfKaMerged512_32_32SCReLU,
    // L1=512, L2=8, L3=96
    HalfKaMerged512CReLU,
    HalfKaMerged512Pairwise,
    HalfKaMerged512SCReLU,
    // L1=768, L2=16, L3=64
    HalfKaMerged768CReLU,
    HalfKaMerged768Pairwise,
    HalfKaMerged768SCReLU,
    // L1=1024, L2=8, L3=32
    HalfKaMerged1024_8_32CReLU,
    HalfKaMerged1024_8_32Pairwise,
    HalfKaMerged1024_8_32SCReLU,
    // L1=1024, L2=8, L3=64
    HalfKaMerged1024_8_64CReLU,
    HalfKaMerged1024_8_64Pairwise,
    HalfKaMerged1024_8_64SCReLU,
    // L1=1024, L2=8, L3=96
    HalfKaMerged1024CReLU,
    HalfKaMerged1024Pairwise,
    HalfKaMerged1024SCReLU,
};

// HalfKaHmSplit 型エイリアス
pub use crate::nnue::network_halfka_hm_split::{
    // L1=256, L2=32, L3=32
    HalfKaHmSplit256CReLU,
    HalfKaHmSplit256Pairwise,
    HalfKaHmSplit256SCReLU,
    // L1=512, L2=8, L3=64
    HalfKaHmSplit512_8_64CReLU,
    HalfKaHmSplit512_8_64Pairwise,
    HalfKaHmSplit512_8_64SCReLU,
    // L1=512, L2=32, L3=32
    HalfKaHmSplit512_32_32CReLU,
    HalfKaHmSplit512_32_32Pairwise,
    HalfKaHmSplit512_32_32SCReLU,
    // L1=512, L2=8, L3=96
    HalfKaHmSplit512CReLU,
    HalfKaHmSplit512Pairwise,
    HalfKaHmSplit512SCReLU,
    // L1=768, L2=16, L3=64
    HalfKaHmSplit768CReLU,
    HalfKaHmSplit768Pairwise,
    HalfKaHmSplit768SCReLU,
    // L1=1024, L2=8, L3=32
    HalfKaHmSplit1024_8_32CReLU,
    HalfKaHmSplit1024_8_32Pairwise,
    HalfKaHmSplit1024_8_32SCReLU,
    // L1=1024, L2=8, L3=64
    HalfKaHmSplit1024_8_64CReLU,
    HalfKaHmSplit1024_8_64Pairwise,
    HalfKaHmSplit1024_8_64SCReLU,
    // L1=1024, L2=8, L3=96
    HalfKaHmSplit1024CReLU,
    HalfKaHmSplit1024Pairwise,
    HalfKaHmSplit1024SCReLU,
};

// HalfKP 型エイリアス
pub use crate::nnue::network_halfkp::{
    // L1=256, L2=32, L3=32
    HalfKP256CReLU,
    HalfKP256Pairwise,
    HalfKP256SCReLU,
    // L1=512, L2=8, L3=64
    HalfKP512_8_64CReLU,
    HalfKP512_8_64Pairwise,
    HalfKP512_8_64SCReLU,
    // L1=512, L2=32, L3=32
    HalfKP512_32_32CReLU,
    HalfKP512_32_32Pairwise,
    HalfKP512_32_32SCReLU,
    // L1=512, L2=8, L3=96
    HalfKP512CReLU,
    HalfKP512Pairwise,
    HalfKP512SCReLU,
    // L1=768, L2=16, L3=64
    HalfKP768CReLU,
    HalfKP768Pairwise,
    HalfKP768SCReLU,
    // L1=1024, L2=8, L3=32
    HalfKP1024_8_32CReLU,
    HalfKP1024_8_32Pairwise,
    HalfKP1024_8_32SCReLU,
    // L1=1024, L2=8, L3=64
    HalfKP1024_8_64CReLU,
    HalfKP1024_8_64Pairwise,
    HalfKP1024_8_64SCReLU,
};
