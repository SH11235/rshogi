use std::{io, str::FromStr};

use super::constants::MAX_LAYER_STACK_BUCKETS;
use crate::{
    position::Position,
    types::{Color, PieceType},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    King {
        opponent: bool,
        bands: [u8; 9],
        radix: u8,
    },
    HandMajor3,
    HandMajor4,
}

impl Axis {
    fn radix(self) -> usize {
        match self {
            Self::King { radix, .. } => usize::from(radix),
            Self::HandMajor3 => 3,
            Self::HandMajor4 => 4,
        }
    }

    fn value(self, pos: &Position) -> usize {
        let us = pos.side_to_move();
        match self {
            Self::King {
                opponent, bands, ..
            } => {
                let color = if opponent { !us } else { us };
                let rank = pos.king_square(color).rank().index();
                let from_home = if color == Color::Black {
                    8 - rank
                } else {
                    rank
                };
                usize::from(bands[from_home])
            }
            Self::HandMajor3 | Self::HandMajor4 => {
                let has_major = |color| {
                    let hand = pos.hand(color);
                    hand.count(PieceType::Rook) != 0 || hand.count(PieceType::Bishop) != 0
                };
                match (has_major(us), has_major(!us)) {
                    (false, false) => 0,
                    (true, false) => 1,
                    (false, true) => 2,
                    (true, true) if self == Self::HandMajor4 => 3,
                    (true, true) => 0,
                }
            }
        }
    }
}

impl FromStr for Axis {
    type Err = String;

    fn from_str(part: &str) -> Result<Self, Self::Err> {
        match part {
            "handmajor3" => return Ok(Self::HandMajor3),
            "handmajor4" => return Ok(Self::HandMajor4),
            _ => {}
        }
        let (name, cuts) = part
            .split_once(':')
            .ok_or("expected ownking:<cuts>, oppking:<cuts>, handmajor3 or handmajor4")?;
        let opponent = match name {
            "ownking" => false,
            "oppking" => true,
            _ => return Err(format!("unknown bucket axis: {name}")),
        };
        let mut bands = [0; 9];
        let mut previous = 0;
        let mut radix = 1;
        for cut in cuts.split('/') {
            let rank: u8 = cut.parse().map_err(|_| format!("invalid king-rank cut: {cut}"))?;
            if !(1..=9).contains(&rank) || rank <= previous {
                return Err("king-rank cuts must be strictly increasing integers in [1, 9]".into());
            }
            bands[usize::from(rank - 1)..].fill(radix);
            radix += 1;
            previous = rank;
        }
        Ok(Self::King {
            opponent,
            bands,
            radix,
        })
    }
}

/// LayerStack の出力 bucket を決める、tatara 互換の 1 軸または 2 軸の指定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BucketSpec {
    first: Axis,
    second: Option<Axis>,
}

impl BucketSpec {
    /// 軸の基数の積を返す。
    pub fn num_buckets(self) -> usize {
        self.first.radix() * self.second.map_or(1, Axis::radix)
    }

    /// 手番を基準に、左の軸を上位桁として bucket index を求める。
    pub fn bucket(self, pos: &Position) -> usize {
        let first = self.first.value(pos);
        self.second.map_or(first, |axis| first * axis.radix() + axis.value(pos))
    }

    pub(super) fn from_arch(arch: &str, stored_buckets: usize) -> io::Result<Option<Self>> {
        let mut tokens = arch.split(',').filter_map(|token| token.strip_prefix("ls_bucket="));
        let Some(value) = tokens.next() else {
            return Ok(None);
        };
        if tokens.next().is_some() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "duplicate ls_bucket token"));
        }
        let spec: Self = value.parse().map_err(|err| {
            io::Error::new(io::ErrorKind::InvalidData, format!("invalid ls_bucket={value}: {err}"))
        })?;
        if spec.num_buckets() != stored_buckets {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "ls_bucket={value} requires {} buckets, but the network stores {stored_buckets}",
                    spec.num_buckets()
                ),
            ));
        }
        Ok(Some(spec))
    }
}

impl FromStr for BucketSpec {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (first, second) = if let Some((first, second)) = text.split_once('+') {
            if second.contains('+') {
                return Err("bucket spec requires one or two axes".into());
            }
            (first.parse()?, Some(second.parse()?))
        } else {
            (text.parse()?, None)
        };
        let spec = Self { first, second };
        if spec.num_buckets() > MAX_LAYER_STACK_BUCKETS {
            return Err(format!(
                "bucket spec needs {} buckets; maximum is {MAX_LAYER_STACK_BUCKETS}",
                spec.num_buckets()
            ));
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tatara_axes_and_bucket_counts() {
        for (text, count) in [
            ("ownking:2/3+oppking:2/3", 9),
            ("ownking:2/3/7+oppking:7", 8),
            ("ownking:2/3/7+oppking:2/3/7", 16),
            ("handmajor4+ownking:2/3/4/5/6/7/8", 32),
            ("ownking:1/2/3/4/5/6/7/8/9", 10),
            ("ownking:2+ownking:4", 4),
            ("handmajor3+handmajor4", 12),
        ] {
            assert_eq!(text.parse::<BucketSpec>().unwrap().num_buckets(), count, "{text}");
        }
        for text in [
            "",
            "spec",
            "handmajor3:",
            "ownking",
            "ownking:",
            "ownking:0",
            "ownking:10",
            "ownking:3/2",
            "ownking:2/2",
            "ownking:2/",
            "ownking:-1",
            "other:2",
            "handmajor4+ownking:2/3/4/5/6/7/8/9",
            "handmajor3+handmajor3+handmajor3",
            "+handmajor3",
            "handmajor3+",
            " ownking:2",
            "oppking:2 ",
        ] {
            assert!(text.parse::<BucketSpec>().is_err(), "{text}");
        }
        assert!(BucketSpec::from_arch("ls_bucket=ownking:2,ls_bucket=oppking:3", 2).is_err());
        assert!(BucketSpec::from_arch("ls_bucket=", 2).is_err());
        assert_eq!(BucketSpec::from_arch("fv_scale=14", 9).unwrap(), None);
    }

    #[test]
    fn king_bands_use_home_ranks_and_hand_axes_use_side_to_move() {
        for (placement, hand, side, nine, sixteen, wide, major3) in [
            ("4k4/9/9/9/9/9/9/9/4K4", "-", "b", 0, 0, 0, 0),
            ("9/4k4/9/9/9/9/9/9/4K4", "R", "b", 1, 1, 8, 1),
            ("9/4k4/9/9/9/9/9/9/4K4", "R", "w", 3, 4, 17, 2),
            ("9/4k4/9/9/9/9/4K4/9/9", "b", "b", 7, 9, 18, 2),
            ("9/4k4/9/9/9/9/4K4/9/9", "b", "w", 5, 6, 9, 1),
            ("4K4/9/9/9/9/9/9/9/4k4", "Rb", "b", 8, 15, 31, 0),
            ("4K4/9/9/9/9/9/9/9/4k4", "Br", "w", 8, 15, 31, 0),
            ("9/9/4K4/9/9/9/9/9/4k4", "Pp", "b", 8, 15, 6, 0),
        ] {
            let sfen = format!("{placement} {side} {hand} 1");
            let mut pos = Position::new();
            pos.set_sfen(&sfen).unwrap();
            for (text, expected) in [
                ("ownking:2/3+oppking:2/3", nine),
                ("ownking:2/3/7+oppking:2/3/7", sixteen),
                ("handmajor4+ownking:2/3/4/5/6/7/8", wide),
                ("handmajor3", major3),
            ] {
                assert_eq!(
                    text.parse::<BucketSpec>().unwrap().bucket(&pos),
                    expected,
                    "{text} {sfen}"
                );
            }
        }
    }
}
