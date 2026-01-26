//! 教師データセット
//!
//! JSONL形式の教師データを読み込み、学習用のバッチを生成する。

use anyhow::{Context, Result};
use rand::seq::SliceRandom;
use rand::Rng;
use rshogi_core::nnue::{halfkp_index, BonaPiece};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType, Square};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// 教師データの1レコード（JSONLから読み込み）
#[derive(Debug, Deserialize)]
pub struct TrainingRecord {
    pub sfen: String,
    pub score: i32,
    #[allow(dead_code)]
    pub depth: i32,
    #[allow(dead_code)]
    pub best_move: String,
    #[allow(dead_code)]
    pub nodes: u64,
}

/// 学習用のサンプル（特徴量抽出済み）
#[derive(Clone)]
pub struct TrainingSample {
    /// 先手視点のアクティブ特徴量インデックス
    pub black_features: Vec<usize>,
    /// 後手視点のアクティブ特徴量インデックス
    pub white_features: Vec<usize>,
    /// 手番（0=先手, 1=後手）
    pub side_to_move: usize,
    /// 目標スコア（センチポーン）
    pub target_score: f32,
}

/// 学習用バッチ
pub struct TrainingBatch {
    pub samples: Vec<TrainingSample>,
}

/// 教師データセット
pub struct TrainingDataset {
    samples: Vec<TrainingSample>,
}

impl TrainingDataset {
    /// JSONLファイルから教師データを読み込み
    pub fn load<P: AsRef<Path>>(path: P, limit: Option<usize>) -> Result<Self> {
        let file = File::open(path.as_ref())
            .with_context(|| format!("Failed to open {}", path.as_ref().display()))?;
        let reader = BufReader::new(file);

        let mut samples = Vec::new();

        for (i, line) in reader.lines().enumerate() {
            if let Some(lim) = limit {
                if samples.len() >= lim {
                    break;
                }
            }

            let line = line.with_context(|| format!("Failed to read line {}", i + 1))?;
            let record: TrainingRecord = serde_json::from_str(&line)
                .with_context(|| format!("Failed to parse line {}: {line}", i + 1))?;

            match Self::record_to_sample(&record) {
                Ok(sample) => samples.push(sample),
                Err(e) => {
                    log::warn!("Skipping line {}: {e}", i + 1);
                }
            }
        }

        Ok(Self { samples })
    }

    /// レコードをサンプルに変換
    fn record_to_sample(record: &TrainingRecord) -> Result<TrainingSample> {
        let mut pos = Position::new();
        pos.set_sfen(&record.sfen)
            .map_err(|e| anyhow::anyhow!("Failed to parse SFEN: {e}"))?;

        let side_to_move = pos.side_to_move().index();

        // 先手視点の特徴量
        let black_king = pos.king_square(Color::Black);
        let black_features = extract_halfkp_features(&pos, Color::Black, black_king);

        // 後手視点の特徴量
        let white_king = pos.king_square(Color::White);
        let white_features = extract_halfkp_features(&pos, Color::White, white_king);

        Ok(TrainingSample {
            black_features,
            white_features,
            side_to_move,
            target_score: record.score as f32,
        })
    }

    /// サンプル数
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// 空かどうか
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// シャッフル
    pub fn shuffle<R: Rng>(&mut self, rng: &mut R) {
        self.samples.shuffle(rng);
    }

    /// バッチを取得
    pub fn get_batch(&self, start: usize, batch_size: usize) -> TrainingBatch {
        let end = (start + batch_size).min(self.samples.len());
        let samples = self.samples[start..end].to_vec();
        TrainingBatch { samples }
    }

    /// エポックのイテレータ
    pub fn batches(&self, batch_size: usize) -> impl Iterator<Item = TrainingBatch> + '_ {
        (0..self.samples.len())
            .step_by(batch_size)
            .map(move |start| self.get_batch(start, batch_size))
    }
}

/// HalfKP特徴量を抽出
fn extract_halfkp_features(pos: &Position, perspective: Color, king_sq: Square) -> Vec<usize> {
    let mut features = Vec::with_capacity(40);

    // 盤上の駒
    for sq_idx in 0..81 {
        let sq = Square::from_u8(sq_idx as u8).unwrap();
        let piece = pos.piece_on(sq);

        if piece.is_none() {
            continue;
        }

        // 玉は特徴量に含めない
        if piece.piece_type() == PieceType::King {
            continue;
        }

        let bp = BonaPiece::from_piece_square(piece, sq, perspective);
        if bp != BonaPiece::ZERO {
            // 視点に応じて玉位置を変換
            let king_sq_idx = if perspective == Color::Black {
                king_sq.index()
            } else {
                king_sq.inverse().index()
            };
            let idx = halfkp_index(Square::from_u8(king_sq_idx as u8).unwrap(), bp);
            features.push(idx);
        }
    }

    // 手駒
    for &color in &[Color::Black, Color::White] {
        for &pt in &[
            PieceType::Pawn,
            PieceType::Lance,
            PieceType::Knight,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
        ] {
            let count = pos.hand(color).count(pt);
            for c in 1..=count {
                let bp = BonaPiece::from_hand_piece(perspective, color, pt, c as u8);
                if bp != BonaPiece::ZERO {
                    let king_sq_idx = if perspective == Color::Black {
                        king_sq.index()
                    } else {
                        king_sq.inverse().index()
                    };
                    let idx = halfkp_index(Square::from_u8(king_sq_idx as u8).unwrap(), bp);
                    features.push(idx);
                }
            }
        }
    }

    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_features() {
        let mut pos = Position::new();
        pos.set_hirate();

        let king_sq = pos.king_square(Color::Black);
        let features = extract_halfkp_features(&pos, Color::Black, king_sq);

        // 初期局面では38駒（玉2枚を除く）
        assert!(!features.is_empty());
        // 最大でも駒数 + 手駒数 程度
        assert!(features.len() <= 40);
    }
}
