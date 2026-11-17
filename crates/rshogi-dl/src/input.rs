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

#[derive(Clone, Copy)]
enum BatchView<'a> {
    Basic(&'a [f32], &'a [f32]),
    Tokens(&'a [u8]),
}

/// 同一形式の完全入力を局面順に並べた読み取り専用 view。
#[derive(Clone, Copy)]
pub struct EncodedBatch<'a> {
    view: BatchView<'a>,
    len: usize,
}

impl<'a> EncodedBatch<'a> {
    /// 局面数。
    pub fn len(self) -> usize {
        self.len
    }
    /// 局面を含まないか。
    pub fn is_empty(self) -> bool {
        self.len == 0
    }
    /// 全局面に共通する入力形式。
    pub fn format(self) -> InputFormat {
        match self.view {
            BatchView::Basic(..) => InputFormat::Basic,
            BatchView::Tokens(_) => InputFormat::StmTokensV1,
        }
    }
    /// basic の平坦な二入力配列。
    pub fn basic(self) -> Option<(&'a [f32], &'a [f32])> {
        match self.view {
            BatchView::Basic(a, b) => Some((a, b)),
            BatchView::Tokens(_) => None,
        }
    }
    /// token の平坦な配列。各局面は TOKEN_COUNT 要素。
    pub fn tokens(self) -> Option<&'a [u8]> {
        match self.view {
            BatchView::Tokens(t) => Some(t),
            BatchView::Basic(..) => None,
        }
    }
    /// 指定した局面の view。範囲外なら None。
    pub fn row(self, index: usize) -> Option<EncodedInput<'a>> {
        if index >= self.len {
            return None;
        }
        Some(match self.view {
            BatchView::Basic(a, b) => EncodedInput::basic_encoded(
                &a[index * FEATURES1_SIZE..(index + 1) * FEATURES1_SIZE],
                &b[index * FEATURES2_SIZE..(index + 1) * FEATURES2_SIZE],
            ),
            BatchView::Tokens(t) => EncodedInput::tokens_encoded(
                t[index * TOKEN_COUNT..(index + 1) * TOKEN_COUNT]
                    .try_into()
                    .expect("fixed token row"),
            ),
        })
    }
}

enum BatchStorage {
    Basic { first: Vec<f32>, second: Vec<f32> },
    Tokens(Vec<u8>),
}

pub(crate) fn zeroed<T: Default + Clone>(len: usize) -> Result<Vec<T>> {
    anyhow::ensure!(
        len.checked_mul(size_of::<T>())
            .is_some_and(|bytes| bytes <= isize::MAX as usize),
        "buffer size overflow"
    );
    let mut result = Vec::new();
    result.try_reserve_exact(len)?;
    result.resize(len, T::default());
    Ok(result)
}

/// 最大局面数を固定し、encode と miss の圧縮に再利用する平坦な入力 buffer。
pub struct InputBatchBuffer {
    storage: BatchStorage,
    capacity: usize,
    len: usize,
}

impl InputBatchBuffer {
    pub(crate) fn required_heap_bytes(format: InputFormat, capacity: usize) -> Result<usize> {
        anyhow::ensure!(capacity > 0, "batch capacity must be positive");
        let row = match format {
            InputFormat::Basic => (FEATURES1_SIZE + FEATURES2_SIZE) * size_of::<f32>(),
            InputFormat::StmTokensV1 => TOKEN_COUNT,
        };
        let bytes = capacity
            .checked_mul(row)
            .ok_or_else(|| anyhow::anyhow!("batch size overflow"))?;
        anyhow::ensure!(bytes <= isize::MAX as usize, "batch size overflow");
        Ok(bytes)
    }
    /// 指定した最大局面数の領域を一度だけ確保する。
    pub fn new(format: InputFormat, capacity: usize) -> Result<Self> {
        Self::required_heap_bytes(format, capacity)?;
        let storage = match format {
            InputFormat::Basic => BatchStorage::Basic {
                first: zeroed(capacity * FEATURES1_SIZE)?,
                second: zeroed(capacity * FEATURES2_SIZE)?,
            },
            InputFormat::StmTokensV1 => BatchStorage::Tokens(zeroed(capacity * TOKEN_COUNT)?),
        };
        Ok(Self {
            storage,
            capacity,
            len: 0,
        })
    }
    /// 有効な局面を破棄する。確保済みの容量は保持する。
    pub fn clear(&mut self) {
        self.len = 0;
    }
    /// 有効な局面数。
    pub fn len(&self) -> usize {
        self.len
    }
    /// 有効な局面がないか。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// 最大局面数。
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// 局面を encode して末尾へ追加する。失敗時は有効長を変更しない。
    pub fn push(&mut self, position: &Position) -> Result<()> {
        anyhow::ensure!(self.len < self.capacity, "batch capacity exceeded");
        match &mut self.storage {
            BatchStorage::Basic { first, second } => {
                let first = &mut first[self.len * FEATURES1_SIZE..(self.len + 1) * FEATURES1_SIZE];
                let second =
                    &mut second[self.len * FEATURES2_SIZE..(self.len + 1) * FEATURES2_SIZE];
                first.fill(0.0);
                second.fill(0.0);
                make_input_features(position, first, second);
            }
            BatchStorage::Tokens(t) => t[self.len * TOKEN_COUNT..(self.len + 1) * TOKEN_COUNT]
                .copy_from_slice(&tokens::encode(position)?),
        }
        self.len += 1;
        Ok(())
    }
    /// 同じ形式の完全入力をコピーして追加する。再 encode は行わない。
    pub fn push_encoded(&mut self, input: EncodedInput<'_>) -> Result<()> {
        anyhow::ensure!(self.len < self.capacity, "batch capacity exceeded");
        match (&mut self.storage, input.view) {
            (BatchStorage::Basic { first, second }, View::Basic(a, b)) => {
                first[self.len * FEATURES1_SIZE..(self.len + 1) * FEATURES1_SIZE]
                    .copy_from_slice(a);
                second[self.len * FEATURES2_SIZE..(self.len + 1) * FEATURES2_SIZE]
                    .copy_from_slice(b);
            }
            (BatchStorage::Tokens(t), View::Tokens(row)) => {
                t[self.len * TOKEN_COUNT..(self.len + 1) * TOKEN_COUNT].copy_from_slice(row)
            }
            _ => anyhow::bail!("encoded input format mismatch"),
        }
        self.len += 1;
        Ok(())
    }
    /// 有効な局面だけを借用する。
    pub fn view(&self) -> EncodedBatch<'_> {
        let view = match &self.storage {
            BatchStorage::Basic { first, second } => BatchView::Basic(
                &first[..self.len * FEATURES1_SIZE],
                &second[..self.len * FEATURES2_SIZE],
            ),
            BatchStorage::Tokens(t) => BatchView::Tokens(&t[..self.len * TOKEN_COUNT]),
        };
        EncodedBatch {
            view,
            len: self.len,
        }
    }
    /// 本体と所有する配列の実 capacity。allocator の管理領域は含まない。
    pub fn allocated_bytes(&self) -> usize {
        size_of::<Self>()
            + match &self.storage {
                BatchStorage::Basic { first, second } => {
                    (first.capacity() + second.capacity()) * size_of::<f32>()
                }
                BatchStorage::Tokens(t) => t.capacity(),
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_preserve_rows_reuse_capacity_and_reject_invalid_pushes() -> Result<()> {
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            assert!(InputBatchBuffer::new(format, 0).is_err());
            assert!(InputBatchBuffer::new(format, usize::MAX).is_err());
            let mut batch = InputBatchBuffer::new(format, 2)?;
            let bytes = batch.allocated_bytes();
            let larger = InputBatchBuffer::new(format, 8)?;
            let row_bytes = match format {
                InputFormat::Basic => (FEATURES1_SIZE + FEATURES2_SIZE) * size_of::<f32>(),
                InputFormat::StmTokensV1 => TOKEN_COUNT,
            };
            assert_eq!(larger.allocated_bytes() - bytes, 6 * row_bytes);
            assert!(batch.is_empty());
            assert!(batch.view().row(0).is_none());
            let mut position = Position::new();
            position.set_sfen("4k4/9/9/9/9/9/9/9/4K4 w 2r2b4g4s4n4l18p 1")?;
            let mut single = InputBuffer::new(format)?;
            let row = single.encode(&position)?;
            batch.push(&position)?;
            batch.push_encoded(row)?;
            for index in 0..2 {
                assert_eq!(batch.view().row(index).unwrap().basic(), row.basic());
                assert_eq!(batch.view().row(index).unwrap().tokens(), row.tokens());
            }
            assert!(batch.view().row(2).is_none());
            assert!(batch.push(&position).is_err());
            assert_eq!(batch.len(), 2);
            batch.clear();
            let other = if format == InputFormat::Basic {
                InputFormat::StmTokensV1
            } else {
                InputFormat::Basic
            };
            let mut other = InputBuffer::new(other)?;
            assert!(batch.push_encoded(other.encode(&position)?).is_err());
            assert!(batch.is_empty());
            batch.push(&position)?;
            assert_eq!(batch.view().len(), 1);
            assert_eq!(batch.allocated_bytes(), bytes);
        }
        Ok(())
    }

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
