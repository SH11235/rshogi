//! PSV (PackedSfenValue, 40B 固定長) を対象にした `GameSource` 実装。
//!
//! 各レコードは局面を丸ごと持つため、連続再生は「次レコードの sfen を
//! decode して表示する」だけで成立する（直前レコードへの move 適用は不要）。
//! 対局境界は `game_ply` のリセット（前レコード以下に戻る）で検出する。

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};

use crate::kif::format_move_label;
use crate::packed_sfen::{PackedSfenValue, move16_to_move, unpack_sfen};

use super::model::{
    GameIndex, GameIndexEntry, GameOutcomeView, GameRecord, GameSource, GameSourceRef,
    MoveAnnotation, MoveView,
};

/// 1対局あたりの平均レコード数がこれを下回ったら、連続した自己対局ストリーム
/// ではない（shuffle_psv 等でシャッフル済みのプールである）可能性を警告する。
/// `skip_in_check` で間引かれた通常の対局でもこの値を大きく下回ることは無い。
const SHORT_GAME_WARNING_THRESHOLD: f64 = 5.0;

pub struct PsvSource {
    path: PathBuf,
}

impl PsvSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl GameSource for PsvSource {
    fn build_index(&self) -> Result<GameIndex> {
        let file = File::open(&self.path)
            .with_context(|| format!("failed to open PSV file {}", self.path.display()))?;
        let mut reader = BufReader::new(file);
        let mut buf = [0u8; PackedSfenValue::SIZE];

        let mut entries = Vec::new();
        let mut prev_ply: Option<u16> = None;
        let mut current_start: u64 = 0;
        let mut record_idx: u64 = 0;
        // 直近に読んだレコードの (game_result, side_to_move)。境界を検出した瞬間、
        // これは「閉じる対局」の最終レコードの値になっている。
        let mut last_result: Option<(i8, Color)> = None;

        loop {
            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e).context("failed to read PSV record"),
            }
            let psv = PackedSfenValue::from_bytes(&buf)
                .ok_or_else(|| anyhow!("corrupt PSV record at index {record_idx}"))?;

            let is_boundary = matches!(prev_ply, Some(p) if psv.game_ply <= p);
            if is_boundary {
                entries.push(finish_entry(
                    current_start,
                    record_idx,
                    last_result,
                    entries.len() as u32,
                ));
                current_start = record_idx;
            }
            prev_ply = Some(psv.game_ply);
            last_result = Some((psv.game_result, side_to_move_from_packed(&psv.sfen)));
            record_idx += 1;
        }

        if record_idx > current_start {
            entries.push(finish_entry(
                current_start,
                record_idx,
                last_result,
                entries.len() as u32,
            ));
        }

        let mut warnings = Vec::new();
        if entries.len() > 1 {
            let avg = record_idx as f64 / entries.len() as f64;
            if avg < SHORT_GAME_WARNING_THRESHOLD {
                warnings.push(format!(
                    "平均対局長が {avg:.1} レコード/対局と極端に短い対局が {} 件検出されました。\
                     shuffle_psv/merge_psv 等でシャッフル済みのプールを指定していないか確認してください。",
                    entries.len()
                ));
            }
        }

        Ok(GameIndex {
            entries,
            pair_files: Vec::new(),
            warnings,
        })
    }

    fn load_game(&self, _index: &GameIndex, entry: &GameIndexEntry) -> Result<GameRecord> {
        let GameSourceRef::Psv {
            start_record,
            end_record,
            ..
        } = entry.source
        else {
            bail!("PsvSource::load_game received a non-PSV GameIndexEntry");
        };
        let mut file = File::open(&self.path)
            .with_context(|| format!("failed to open PSV file {}", self.path.display()))?;
        file.seek(SeekFrom::Start(start_record * PackedSfenValue::SIZE as u64))?;

        let count = (end_record - start_record) as usize;
        let mut moves = Vec::with_capacity(count);
        let mut buf = [0u8; PackedSfenValue::SIZE];
        for _ in 0..count {
            file.read_exact(&mut buf)
                .context("failed to read PSV record while loading game")?;
            let psv = PackedSfenValue::from_bytes(&buf)
                .ok_or_else(|| anyhow!("corrupt PSV record while loading game"))?;

            let sfen_before =
                unpack_sfen(&psv.sfen).map_err(|e| anyhow!("failed to unpack PSV sfen: {e}"))?;
            let mut pos = Position::new();
            pos.set_sfen(&sfen_before)
                .map_err(|e| anyhow!("failed to parse PSV sfen '{sfen_before}': {e}"))?;
            let side = pos.side_to_move();

            // move16 == 0 は「この局面からの指し手は記録されていない」
            // （対局終了時点の最終レコード等）を意味し、Move::PASS とは別物。
            // format_move_label に通常の指し手として渡すと無意味な座標を
            // 表示してしまうため、専用ラベルにする。
            let (mv, kif_label) = if psv.move16 == 0 {
                (Move::NONE, format!("{:>4} (終局局面)", psv.game_ply))
            } else {
                let mv = move16_to_move(psv.move16);
                let label = format_move_label(psv.game_ply as u32, &pos, mv);
                (mv, label)
            };

            moves.push(MoveView {
                ply: psv.game_ply as u32,
                side,
                sfen_before,
                mv,
                kif_label,
                annotation: MoveAnnotation {
                    score_cp: Some(psv.score as i32),
                    ..Default::default()
                },
            });
        }

        Ok(GameRecord { moves })
    }
}

/// packed sfen の bit 0（手番）だけを読む。インデックス構築フェーズでは
/// 盤面全体のハフマン復号は不要なため、`unpack_sfen` を呼ばずに済ませる。
fn side_to_move_from_packed(sfen: &[u8; 32]) -> Color {
    if sfen[0] & 1 == 0 {
        Color::Black
    } else {
        Color::White
    }
}

fn finish_entry(
    start_record: u64,
    end_record: u64,
    last_result: Option<(i8, Color)>,
    ordinal: u32,
) -> GameIndexEntry {
    let outcome = match last_result {
        Some((1, side)) => Some(GameOutcomeView::Win(side)),
        Some((-1, side)) => Some(GameOutcomeView::Win(!side)),
        Some((0, _)) => Some(GameOutcomeView::Draw),
        _ => None,
    };
    GameIndexEntry {
        source: GameSourceRef::Psv {
            start_record,
            end_record,
            ordinal,
        },
        outcome,
        error: false,
        ply_count: (end_record - start_record) as u32,
        pair_index: None,
        pair_slot: None,
        startpos_idx: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// game_ply / score / move16 / game_result を指定してテスト用 PSV レコードを作る。
    /// 盤面は平手初期局面の packed sfen を使い回し、bit 0 (手番) だけ書き換える
    /// （全 0 埋めだと先手玉・後手玉が同一マス扱いになり `unpack_sfen` が失敗するため）。
    fn record(side: Color, game_ply: u16, score: i16, move16: u16, game_result: i8) -> [u8; 40] {
        let mut hirate = Position::new();
        hirate
            .set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let mut sfen = crate::packed_sfen::pack_position(&hirate);
        if side == Color::White {
            sfen[0] |= 1;
        } else {
            sfen[0] &= !1;
        }
        let psv = PackedSfenValue {
            sfen,
            score,
            move16,
            game_ply,
            game_result,
            padding: 0,
        };
        psv.to_bytes()
    }

    fn write_psv(records: &[[u8; 40]]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        for r in records {
            f.write_all(r).expect("write record");
        }
        f.flush().expect("flush");
        f
    }

    #[test]
    fn build_index_splits_on_ply_reset() {
        let records = [
            record(Color::Black, 1, 0, 1, 0),
            record(Color::White, 2, 0, 1, 0),
            record(Color::Black, 3, 0, 0, 1), // 対局1終了（黒勝ち、手番=黒視点 +1）
            record(Color::Black, 1, 0, 1, 0), // 対局2開始
            record(Color::White, 2, 0, 0, -1), // 対局2終了（白勝ち：手番=白視点 -1）
        ];
        let f = write_psv(&records);
        let source = PsvSource::new(f.path());
        let index = source.build_index().expect("build_index");

        assert_eq!(index.entries.len(), 2);
        let g1 = &index.entries[0];
        assert_eq!(g1.ply_count, 3);
        assert_eq!(g1.outcome, Some(GameOutcomeView::Win(Color::Black)));
        let g2 = &index.entries[1];
        assert_eq!(g2.ply_count, 2);
        // game_result=-1 はそのレコードの手番(白)から見て負け = 勝者は黒。
        assert_eq!(g2.outcome, Some(GameOutcomeView::Win(Color::Black)));
    }

    #[test]
    fn build_index_handles_draw() {
        let records = [
            record(Color::Black, 1, 0, 1, 0),
            record(Color::White, 2, 0, 0, 0),
        ];
        let f = write_psv(&records);
        let index = PsvSource::new(f.path()).build_index().expect("build_index");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].outcome, Some(GameOutcomeView::Draw));
    }

    #[test]
    fn load_game_reads_only_requested_range() {
        let records = [
            record(Color::Black, 1, 10, 1, 0),
            record(Color::White, 2, -20, 0, 1),
            record(Color::Black, 1, 30, 1, 0),
        ];
        let f = write_psv(&records);
        let source = PsvSource::new(f.path());
        let index = source.build_index().expect("build_index");
        assert_eq!(index.entries.len(), 2);

        let game1 = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game1.moves.len(), 2);
        assert_eq!(game1.moves[0].annotation.score_cp, Some(10));
        assert_eq!(game1.moves[1].annotation.score_cp, Some(-20));

        let game2 = source.load_game(&index, &index.entries[1]).expect("load_game");
        assert_eq!(game2.moves.len(), 1);
    }

    #[test]
    fn warns_on_implausibly_short_average_game_length() {
        // 全レコードが ply=1 (常にリセット扱い) = 1局1レコードのシャッフル済みプール相当。
        let records: Vec<[u8; 40]> = (0..20).map(|_| record(Color::Black, 1, 0, 1, 0)).collect();
        let f = write_psv(&records);
        let index = PsvSource::new(f.path()).build_index().expect("build_index");
        assert_eq!(index.entries.len(), 20);
        assert!(!index.warnings.is_empty(), "shuffled pool であることが警告されるべき");
    }
}
