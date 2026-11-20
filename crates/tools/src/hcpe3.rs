//! HCPE3 の対局ヘッダー、着手ヘッダー、候補手を順に読む。

use std::io::{self, Read};

use rshogi_core::position::Position;

use crate::packed_sfen::unpack_hcp;

const GAME_HEADER_SIZE: usize = 36;
const RESULT_MASK: u8 = 0b11;

/// 開始局面、手数、終局理由を除いた勝敗。
pub struct GameHeader {
    pub hcp: [u8; 32],
    pub move_count: u16,
    pub result: u8,
}

/// 実着手、評価値、直後に続く候補手の数。
pub struct MoveHeader {
    pub move16: u16,
    pub eval: i16,
    pub candidate_count: u16,
}

/// 対局の境界での EOF だけを正常終了として扱う。
pub fn read_game_header(reader: &mut impl Read) -> io::Result<Option<GameHeader>> {
    let mut header = [0u8; GAME_HEADER_SIZE];
    let mut filled = 0;
    while filled < GAME_HEADER_SIZE {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("対局ヘッダが途中で終わっています ({filled}/{GAME_HEADER_SIZE} byte)"),
                ));
            }
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let mut hcp = [0u8; 32];
    hcp.copy_from_slice(&header[..32]);
    let result = header[34] & RESULT_MASK;
    if result > 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("不正な勝敗値: result={:#04x}", header[34]),
        ));
    }
    Ok(Some(GameHeader {
        hcp,
        move_count: u16::from_le_bytes([header[32], header[33]]),
        result,
    }))
}

/// 次の着手ヘッダーを読み、候補手列の手前に進める。
pub fn read_move_header(reader: &mut impl Read) -> io::Result<MoveHeader> {
    let mut info = [0u8; 6];
    reader.read_exact(&mut info)?;
    Ok(MoveHeader {
        move16: u16::from_le_bytes([info[0], info[1]]),
        eval: i16::from_le_bytes([info[2], info[3]]),
        candidate_count: u16::from_le_bytes([info[4], info[5]]),
    })
}

/// 候補手を一つ読み、HCPE3 の着手コードと訪問数を返す。
pub fn read_candidate(reader: &mut impl Read) -> io::Result<(u16, u16)> {
    let mut entry = [0u8; 4];
    reader.read_exact(&mut entry)?;
    Ok((
        u16::from_le_bytes([entry[0], entry[1]]),
        u16::from_le_bytes([entry[2], entry[3]]),
    ))
}

/// 訪問数を保存しない変換向けに、候補手列を検査しながら読み飛ばす。
pub fn skip_candidates(reader: &mut impl Read, count: u16) -> io::Result<()> {
    let skip = u64::from(count) * 4;
    let skipped = io::copy(&mut reader.by_ref().take(skip), &mut io::sink())?;
    if skipped != skip {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "候補手の列が途中で終わっています",
        ));
    }
    Ok(())
}

/// HCP を復元し、対局の開始局面を返す。
pub fn start_position(hcp: &[u8; 32]) -> io::Result<Position> {
    let sfen = unpack_hcp(hcp).map_err(|error| {
        io::Error::new(io::ErrorKind::InvalidData, format!("HuffmanCodedPos デコード失敗: {error}"))
    })?;
    let sfen = format!("{} 1", sfen.rsplit_once(' ').map(|(prefix, _)| prefix).unwrap_or(&sfen));
    let mut pos = Position::new();
    pos.set_sfen(&sfen).map_err(|error| {
        io::Error::new(io::ErrorKind::InvalidData, format!("SFEN パース失敗 ({sfen}): {error}"))
    })?;
    Ok(pos)
}
