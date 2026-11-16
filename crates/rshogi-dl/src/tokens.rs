//! 手番正規化した盤面と持駒を表す stm-tokens-v1 入力。

use anyhow::{Result, ensure};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType};

/// 81 マス・双方 7 種の持駒・元の先後の合計。
pub const TOKEN_COUNT: usize = 96;
/// stm-tokens-v1 が受理する token ID の総数。
pub const VOCAB_SIZE: usize = 121;

const HAND_TYPES: [PieceType; 7] = [
    PieceType::Pawn,
    PieceType::Lance,
    PieceType::Knight,
    PieceType::Silver,
    PieceType::Bishop,
    PieceType::Rook,
    PieceType::Gold,
];
const HAND_MAX: [u32; 7] = [18, 4, 4, 4, 2, 2, 4];
const HAND_OFFSET: [u8; 7] = [0, 19, 24, 29, 34, 37, 40];

fn piece_id(piece: PieceType) -> u8 {
    // 内部 enum の並び替えで保存済みモデルの入力契約を変えない。
    match piece {
        PieceType::Pawn => 1,
        PieceType::Lance => 2,
        PieceType::Knight => 3,
        PieceType::Silver => 4,
        PieceType::Bishop => 5,
        PieceType::Rook => 6,
        PieceType::Gold => 7,
        PieceType::King => 8,
        PieceType::ProPawn => 9,
        PieceType::ProLance => 10,
        PieceType::ProKnight => 11,
        PieceType::ProSilver => 12,
        PieceType::Horse => 13,
        PieceType::Dragon => 14,
    }
}

fn hand_token(side: usize, kind: usize, count: u32) -> Result<u8> {
    ensure!(
        count <= HAND_MAX[kind],
        "hand count exceeds stm-tokens-v1 maximum: {:?}={count}",
        HAND_TYPES[kind]
    );
    Ok(29 + side as u8 * 45 + HAND_OFFSET[kind] + count as u8)
}

/// 持駒を切り詰めず、上限を超える局面はエラーにする。履歴・手数は含めない。
pub fn encode(position: &Position) -> Result<[u8; TOKEN_COUNT]> {
    let mut tokens = [0; TOKEN_COUNT];
    let turn = position.side_to_move();
    for square in position.occupied().iter() {
        let piece = position.piece_on(square);
        let index = if turn == Color::Black {
            square.index()
        } else {
            square.inverse().index()
        };
        tokens[index] = piece_id(piece.piece_type()) + if piece.color() == turn { 0 } else { 14 };
    }
    for (side, color) in [turn, turn.opponent()].into_iter().enumerate() {
        let hand = position.hand(color);
        for (kind, piece) in HAND_TYPES.into_iter().enumerate() {
            let count = hand.count(piece);
            tokens[81 + side * 7 + kind] = hand_token(side, kind, count)?;
        }
    }
    // 宣言勝ちの閾値が先後で異なるため、正規化前の手番を残す。
    tokens[95] = if turn == Color::Black { 119 } else { 120 };
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_sfen(sfen: &str) -> Result<Position> {
        let mut position = Position::new();
        position.set_sfen(sfen)?;
        Ok(position)
    }

    #[test]
    fn all_piece_ids_and_both_original_sides() -> Result<()> {
        for (board, ids) in [
            ("plnsbrgk1/9/9/9/9/9/9/9/1KGRBSNLP", 1u8..=8),
            ("4k4/+p+l+n+s+b+r3/9/9/9/9/9/3+R+B+S+N+L+P/4K4", 9u8..=14),
        ] {
            let mut expected = [0; TOKEN_COUNT];
            if *ids.start() == 9 {
                expected[4 * 9 + 8] = 8;
                expected[4 * 9] = 22;
            }
            for id in ids {
                let index = if id <= 8 {
                    (id as usize - 1) * 9 + 8
                } else {
                    (id as usize - 9) * 9 + 7
                };
                expected[index] = id;
                expected[80 - index] = 14 + id;
            }
            expected[81..95]
                .copy_from_slice(&[29, 48, 53, 58, 63, 66, 69, 74, 93, 98, 103, 108, 111, 114]);
            for (side, context) in [("b", 119), ("w", 120)] {
                expected[95] = context;
                assert_eq!(encode(&from_sfen(&format!("{board} {side} - 1"))?)?, expected);
            }
        }
        Ok(())
    }

    #[test]
    fn exact_hand_counts_and_policy_drop_order() -> Result<()> {
        let position = from_sfen("4k4/9/9/9/9/9/9/9/4K4 b 9P2L3NS2B2R3G8p2ln3sg 1")?;
        let black = encode(&position)?;
        assert_eq!(&black[81..95], &[38, 50, 56, 59, 65, 68, 72, 82, 95, 99, 106, 108, 111, 115]);
        let white = encode(&from_sfen(&position.to_sfen().replace(" b ", " w "))?)?;
        for kind in 0..7 {
            assert_eq!(black[81 + kind] + 45, white[88 + kind]);
            assert_eq!(black[88 + kind] - 45, white[81 + kind]);
        }
        for count in [0, 8, 9, 18] {
            let hand = if count == 0 {
                "-".to_owned()
            } else {
                format!("{count}P")
            };
            let position = from_sfen(&format!("4k4/9/9/9/9/9/9/9/4K4 b {hand} 1"))?;
            assert_eq!(encode(&position)?[81], 29 + count);
        }
        let maximum = encode(&from_sfen("4k4/9/9/9/9/9/9/9/4K4 b 18P4L4N4S4G2B2R 1")?)?;
        assert_eq!(&maximum[81..88], &[47, 52, 57, 62, 65, 68, 73]);
        Ok(())
    }

    #[test]
    fn sfen_roundtrip_and_ply_independence() -> Result<()> {
        let position = from_sfen("4k4/9/1+p7/9/9/6B2/9/9/4K4 b 2P3l 42")?;
        let tokens = encode(&position)?;
        assert_eq!(tokens[7 * 9 + 2], 23);
        assert_eq!(tokens[2 * 9 + 5], 5);
        assert_eq!(tokens, encode(&from_sfen(&position.to_sfen())?)?);
        assert_eq!(tokens, encode(&from_sfen(&position.to_sfen().replace(" 42", " 999"))?)?);
        let rotated = from_sfen("4k4/9/9/2b6/9/9/7+P1/9/4K4 w 3L2p 42")?;
        let rotated_tokens = encode(&rotated)?;
        assert_eq!(&tokens[..95], &rotated_tokens[..95]);
        assert_eq!((tokens[95], rotated_tokens[95]), (119, 120));
        Ok(())
    }

    #[test]
    fn out_of_range_hand_is_rejected() {
        for (kind, maximum) in HAND_MAX.into_iter().enumerate() {
            for side in 0..2 {
                assert!(hand_token(side, kind, maximum + 1).is_err());
                assert!(hand_token(side, kind, maximum).is_ok());
            }
        }
    }
}
