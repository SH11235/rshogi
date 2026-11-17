//! 局面から作った完全入力を、再 encode せず推論へ渡す借用 view。

use std::mem::size_of;

use anyhow::Result;
use rshogi_core::position::Position;

use crate::InputFormat;
use crate::features::{FEATURES1_SIZE, FEATURES2_SIZE, make_input_features};
use crate::tokens::{self, TOKEN_COUNT};

#[derive(Clone, Copy)]
enum View<'a> {
    Basic(&'a [f32], &'a [f32]),
    Tokens(&'a [u8; TOKEN_COUNT]),
}

/// encoder が作った完全入力への読み取り専用 view。元の buffer より長く保持できない。
#[derive(Clone, Copy)]
pub struct EncodedInput<'a> {
    view: View<'a>,
}

impl<'a> EncodedInput<'a> {
    pub(crate) fn basic_encoded(first: &'a [f32], second: &'a [f32]) -> Self {
        Self {
            view: View::Basic(first, second),
        }
    }

    pub(crate) fn tokens_encoded(tokens: &'a [u8; TOKEN_COUNT]) -> Self {
        Self {
            view: View::Tokens(tokens),
        }
    }

    /// モデルへ渡す完全入力の形式。
    pub fn format(self) -> InputFormat {
        match self.view {
            View::Basic(..) => InputFormat::Basic,
            View::Tokens(_) => InputFormat::StmTokensV1,
        }
    }

    /// basic 形式なら盤面・利き plane と持駒 plane を返す。
    pub fn basic(self) -> Option<(&'a [f32], &'a [f32])> {
        match self.view {
            View::Basic(first, second) => Some((first, second)),
            View::Tokens(_) => None,
        }
    }

    /// token 形式なら正確な持駒と元の先後を含む token 列を返す。
    pub fn tokens(self) -> Option<&'a [u8; TOKEN_COUNT]> {
        match self.view {
            View::Basic(..) => None,
            View::Tokens(tokens) => Some(tokens),
        }
    }
}

enum Storage {
    Basic { first: Vec<f32>, second: Vec<f32> },
    Tokens([u8; TOKEN_COUNT]),
}

/// 一度確保した形式別 buffer を繰り返し encode に使う。
pub struct InputBuffer {
    storage: Storage,
}

impl InputBuffer {
    /// 形式を固定して必要な buffer を確保する。容量不足はエラーで返す。
    pub fn new(format: InputFormat) -> Result<Self> {
        let storage = match format {
            InputFormat::Basic => {
                let mut first = Vec::new();
                first.try_reserve_exact(FEATURES1_SIZE)?;
                first.resize(FEATURES1_SIZE, 0.0);
                let mut second = Vec::new();
                second.try_reserve_exact(FEATURES2_SIZE)?;
                second.resize(FEATURES2_SIZE, 0.0);
                Storage::Basic { first, second }
            }
            InputFormat::StmTokensV1 => Storage::Tokens([0; TOKEN_COUNT]),
        };
        Ok(Self { storage })
    }

    /// 完全入力を作って view を返す。失敗時は view を返さず、次回も buffer を再利用できる。
    pub fn encode(&mut self, position: &Position) -> Result<EncodedInput<'_>> {
        match &mut self.storage {
            Storage::Basic { first, second } => {
                first.fill(0.0);
                second.fill(0.0);
                make_input_features(position, first, second);
                Ok(EncodedInput::basic_encoded(first, second))
            }
            Storage::Tokens(output) => {
                *output = tokens::encode(position)?;
                Ok(EncodedInput::tokens_encoded(output))
            }
        }
    }

    /// buffer 本体と所有する Vec の実 capacity の合計。allocator の管理領域は含まない。
    pub fn allocated_bytes(&self) -> usize {
        size_of::<Self>()
            + match &self.storage {
                Storage::Basic { first, second } => {
                    (first.capacity() + second.capacity()) * size_of::<f32>()
                }
                Storage::Tokens(_) => 0,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reusable_views_match_both_canonical_encoders() -> Result<()> {
        let mut basic = InputBuffer::new(InputFormat::Basic)?;
        let mut token = InputBuffer::new(InputFormat::StmTokensV1)?;
        let basic_bytes = basic.allocated_bytes();
        let token_bytes = token.allocated_bytes();
        assert_eq!(token_bytes, size_of::<InputBuffer>());
        let mut position = Position::new();
        for sfen in [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "4k4/9/9/9/9/9/9/9/4K4 w 2r2b4g4s4n4l18p 1",
            "4k4/9/9/9/9/9/9/9/4K4 b - 37",
        ] {
            position.set_sfen(sfen)?;
            let mut first = vec![0.0; FEATURES1_SIZE];
            let mut second = vec![0.0; FEATURES2_SIZE];
            make_input_features(&position, &mut first, &mut second);
            let input = basic.encode(&position)?;
            assert_eq!(input.format(), InputFormat::Basic);
            assert_eq!(input.basic(), Some((first.as_slice(), second.as_slice())));
            assert!(input.tokens().is_none());
            let input = token.encode(&position)?;
            assert_eq!(input.format(), InputFormat::StmTokensV1);
            assert_eq!(input.tokens(), Some(&tokens::encode(&position)?));
            assert!(input.basic().is_none());
        }
        assert_eq!(basic.allocated_bytes(), basic_bytes);
        assert_eq!(token.allocated_bytes(), token_bytes);
        Ok(())
    }
}
