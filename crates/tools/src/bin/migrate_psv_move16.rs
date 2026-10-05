//! 旧リポジトリ内部形式 (B) の PSV move16 を実 YaneuraOu 形式 (A) へ移行する。

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use rshogi_core::position::Position;
use rshogi_core::types::Move;
use tools::common::io::partial_path;
use tools::output_path::ensure_safe_output_path;
use tools::packed_sfen::{
    PackedSfenValue, PsvMove16Class, classify_psv_move16, is_legal_psv_move, legacy_move16_to_move,
    move_to_psv_move16, unpack_sfen_to_parts,
};

const IO_BUF_SIZE: usize = 1 << 20;
const FORMAT_SCAN_RECORDS: u64 = 100_000;

#[derive(Parser, Debug)]
#[command(
    name = "migrate_psv_move16",
    about = "旧リポジトリ形式 (B) の PSV move16 を実 YaneuraOu 形式 (A) へ移行"
)]
struct Cli {
    /// 入力 PSV ファイル
    #[arg(long)]
    input: PathBuf,

    /// 出力 PSV ファイル
    #[arg(long)]
    output: PathBuf,

    /// 各局面でデコードした指し手の合法性を検証（`--verify-legal=false` で形式確認のみの高速移行）
    #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
    verify_legal: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct FormatEvidence {
    legacy_or_hcpe: bool,
    bit15: bool,
    hcpe_pawn_drop: bool,
}

fn inspect_move16s(move16s: impl IntoIterator<Item = u16>) -> FormatEvidence {
    let mut evidence = FormatEvidence::default();
    for move16 in move16s {
        let from = (move16 >> 7) & 0x7f;
        let drop_bit = move16 & 0x4000 != 0;
        evidence.bit15 |= move16 & 0x8000 != 0;
        evidence.hcpe_pawn_drop |= move16 != 0 && !drop_bit && from == 81;
        evidence.legacy_or_hcpe |= classify_psv_move16(move16) == PsvMove16Class::LegacyOrHcpe;
    }
    evidence
}

fn validate_format_evidence(evidence: &FormatEvidence, scanned: u64) -> Result<()> {
    if evidence.bit15 {
        anyhow::bail!("入力には bit15 の move16 があり、既に実 YaneuraOu 形式 (A) です");
    }
    if evidence.hcpe_pawn_drop {
        anyhow::bail!("入力には from=81 の hcpe 形式 (C) の駒打ちが混入しています");
    }
    if !evidence.legacy_or_hcpe {
        anyhow::bail!("先頭 {scanned} レコードから旧リポジトリ形式 (B) を確認できません");
    }
    Ok(())
}

fn validate_input_format(path: &Path, records: u64) -> Result<()> {
    let file = File::open(path).with_context(|| format!("{} を開けません", path.display()))?;
    let mut reader = BufReader::with_capacity(IO_BUF_SIZE, file);
    let mut record = [0u8; PackedSfenValue::SIZE];
    let mut move16s = Vec::with_capacity(records.min(FORMAT_SCAN_RECORDS) as usize);
    for _ in 0..records.min(FORMAT_SCAN_RECORDS) {
        reader.read_exact(&mut record)?;
        move16s.push(u16::from_le_bytes([record[34], record[35]]));
    }
    let evidence = inspect_move16s(move16s);
    validate_format_evidence(&evidence, records.min(FORMAT_SCAN_RECORDS))
}

fn convert_record_move16(
    mut record: [u8; PackedSfenValue::SIZE],
) -> ([u8; PackedSfenValue::SIZE], Move) {
    let legacy = u16::from_le_bytes([record[34], record[35]]);
    let mv = legacy_move16_to_move(legacy);
    record[34..36].copy_from_slice(&move_to_psv_move16(mv).to_le_bytes());
    (record, mv)
}

fn is_legal_move(psv: &PackedSfenValue, mv: Move) -> bool {
    if mv.is_none() {
        return psv.move16 == 0;
    }
    let Ok(parts) = unpack_sfen_to_parts(&psv.sfen) else {
        return false;
    };
    let mut pos = Position::new();
    if pos.set_from_parts(&parts.board, &parts.hands, parts.side_to_move).is_err() {
        return false;
    }
    is_legal_psv_move(&pos, mv)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let metadata = std::fs::metadata(&cli.input)
        .with_context(|| format!("{} の情報を取得できません", cli.input.display()))?;
    if metadata.len() % PackedSfenValue::SIZE as u64 != 0 {
        anyhow::bail!(
            "入力サイズ {} が PSV レコード長 {} の倍数ではありません",
            metadata.len(),
            PackedSfenValue::SIZE
        );
    }
    let records = metadata.len() / PackedSfenValue::SIZE as u64;
    validate_input_format(&cli.input, records)?;

    let tmp_output = partial_path(&cli.output);
    ensure_safe_output_path(&cli.output, &cli.input)?;
    ensure_safe_output_path(&tmp_output, &cli.input)?;

    let input = File::open(&cli.input)?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_output)
        .with_context(|| {
            format!(
                "{} を新規作成できません。既存の .partial は内容と使用中でないことを確認してから移動または削除してください",
                tmp_output.display()
            )
        })?;
    let mut reader = BufReader::with_capacity(IO_BUF_SIZE, input);
    let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, output);
    let mut record = [0u8; PackedSfenValue::SIZE];
    let mut legal_errors = 0u64;

    for index in 0..records {
        reader.read_exact(&mut record)?;
        let psv = PackedSfenValue::from_bytes(&record).expect("固定長レコード");
        let (converted, mv) = convert_record_move16(record);
        if cli.verify_legal && !is_legal_move(&psv, mv) {
            legal_errors += 1;
            eprintln!("合法手検証エラー: レコード {} move16=0x{:04x}", index + 1, psv.move16);
        }
        writer.write_all(&converted)?;
    }
    writer.flush()?;
    drop(writer);
    std::fs::rename(&tmp_output, &cli.output).with_context(|| {
        format!("{} → {} の rename に失敗", tmp_output.display(), cli.output.display())
    })?;

    eprintln!("変換レコード数: {records}");
    eprintln!("合法手検証エラー: {legal_errors}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::types::{PieceType, Square};

    #[test]
    fn legacy_move_is_converted_to_psv() {
        let mv = Move::new_drop(PieceType::Pawn, Square::SQ_55);
        let legacy = tools::packed_sfen::move_to_legacy_move16(mv);
        let mut record = [0x5au8; PackedSfenValue::SIZE];
        record[34..36].copy_from_slice(&legacy.to_le_bytes());
        let (converted, decoded) = convert_record_move16(record);
        assert_eq!(decoded, mv);
        assert_eq!(u16::from_le_bytes([converted[34], converted[35]]), 40 | (1 << 7) | 0x4000);
        assert_eq!(&converted[..34], &record[..34]);
        assert_eq!(&converted[36..], &record[36..]);
        assert!(inspect_move16s([legacy]).legacy_or_hcpe);
    }

    #[test]
    fn a_input_is_detected() {
        let promoted = 10 | (11 << 7) | 0x8000;
        let evidence = inspect_move16s([promoted]);
        assert!(validate_format_evidence(&evidence, 1).is_err());
    }
}
