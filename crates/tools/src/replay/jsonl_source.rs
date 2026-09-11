//! tournament JSONL (`pair-{i}-{j}__{label_i}-vs-{label_j}.jsonl`、旧形式も対応) の `GameSource` 実装。
//!
//! out-dir 配下のカードファイルを横断して、対局単位の索引を1つのリストに
//! フラット化する。`game_id` はペアファイルごとのローカル連番（out-dir 全体での
//! 一意性は無い）なので、一意キーは `(file_idx, game_id)` にする。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};
use serde::Deserialize;
use serde_json::Value;

use crate::kif::format_move_label;
use crate::selfplay::EvalLog;

use super::model::{
    EvalAccumulator, GameIndex, GameIndexEntry, GameOutcomeView, GameRecord, GameSource,
    GameSourceRef, MoveAnnotation, MoveView, PairFileMeta, date_key_from_filename,
    fingerprint_paths,
};

pub struct JsonlSource {
    out_dir: PathBuf,
}

impl JsonlSource {
    pub fn new(out_dir: impl Into<PathBuf>) -> Self {
        Self {
            out_dir: out_dir.into(),
        }
    }

    fn collect_paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&self.out_dir)
            .with_context(|| format!("failed to read directory {}", self.out_dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_pair_jsonl_name(p))
            .collect();
        // file_idx を実行のたびに安定させるため、列挙順をファイル名でソートする。
        paths.sort();
        Ok(paths)
    }
}

impl GameSource for JsonlSource {
    fn build_index(&self) -> Result<GameIndex> {
        let paths = self.collect_paths()?;

        let mut entries = Vec::new();
        let mut pair_files = Vec::new();
        let mut warnings = Vec::new();

        for path in &paths {
            let file_idx = pair_files.len();
            // スキップ理由ごとの warning は index_one_file 側で push 済みなので、
            // ここでは Some/None の振り分けだけ行う（二重・不正確な warning を防ぐ）。
            if let Some(meta) = index_one_file(path, file_idx, &mut entries, &mut warnings)? {
                pair_files.push(meta);
            }
        }

        Ok(GameIndex {
            entries,
            pair_files,
            warnings,
        })
    }

    fn live_fingerprint(&self) -> Result<Option<u64>> {
        Ok(Some(fingerprint_paths(&self.collect_paths()?)))
    }

    fn load_game(&self, index: &GameIndex, entry: &GameIndexEntry) -> Result<GameRecord> {
        let GameSourceRef::Jsonl {
            file_idx,
            game_id,
            start_offset,
            end_offset,
        } = entry.source
        else {
            bail!("JsonlSource::load_game received a non-JSONL GameIndexEntry");
        };
        let meta = index
            .pair_file(file_idx)
            .ok_or_else(|| anyhow!("file_idx {file_idx} not found in index"))?;

        let mut file = File::open(&meta.path)
            .with_context(|| format!("failed to open {}", meta.path.display()))?;
        file.seek(SeekFrom::Start(start_offset))?;
        let mut buf = vec![0u8; (end_offset - start_offset) as usize];
        file.read_exact(&mut buf).context("failed to read JSONL game byte range")?;

        let mut moves = Vec::new();
        for line in buf.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_slice(line)
                .context("failed to parse JSONL line while loading game")?;
            if value.get("type").and_then(Value::as_str) != Some("move") {
                continue; // result 行はここでは不要
            }
            let move_line: MoveLine = serde_json::from_value(value)
                .context("failed to parse move line while loading game")?;
            // インデックス時のバイト範囲が他 game の行を巻き込んでいないことの
            // 防御的検証。現行の tournament.rs 書き込みモデルでは起こらない
            // はずだが、将来の書き込みモデル変更や手作業で連結したファイル等で
            // 不変条件が崩れた場合に、誤った対局として静かに表示しないため。
            if move_line.game_id != game_id {
                bail!(
                    "{}: 範囲 [{start_offset}, {end_offset}) に game_id={} の move 行が \
                     混入しています（期待値 game_id={game_id}）。索引が壊れている可能性があります。",
                    meta.path.display(),
                    move_line.game_id
                );
            }
            moves.push(build_move_view(&move_line)?);
        }

        // JSONL は定跡途中開始でも絶対手数を持つ正当な開始なので、先頭手数>1 を
        // 欠落として扱わない。
        Ok(GameRecord {
            moves,
            leading_gap_is_drop: false,
            // JSONL は CSA 特殊手を持たないため終局理由は常に不明。
            termination: None,
        })
    }
}

/// tournament の index 形式・旧ラベル形式のペアファイルと csa_client の per-game 記録
/// (`{datetime}_{sente}_vs_{gote}.jsonl`)の両方を対局ファイルとして扱う
/// (スキーマは共通で、meta 行の検証は `index_one_file` が行う)。
fn is_pair_jsonl_name(path: &Path) -> bool {
    let Some(stem) =
        path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".jsonl"))
    else {
        return false;
    };
    if stem.contains("-vs-") || stem.contains("_vs_") {
        return true;
    }
    let Some((first, second)) = stem.strip_prefix("pair-").and_then(|s| s.split_once('-')) else {
        return false;
    };
    [first, second]
        .iter()
        .all(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Deserialize)]
struct MetaLine {
    engine_cmd: MetaEngineCmd,
}

#[derive(Deserialize)]
struct MetaEngineCmd {
    label_black: String,
    label_white: String,
}

#[derive(Deserialize)]
struct MoveLine {
    game_id: u32,
    // JSONL の `ply` は対局内 1 始まりで絶対手数ではないため使わない（絶対手数は
    // `sfen_before` の SFEN 手数カウンタから `build_move_view` で取り直す）。
    sfen_before: String,
    move_usi: String,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    timed_out: Option<bool>,
    #[serde(default)]
    eval: Option<EvalLog>,
}

#[derive(Deserialize)]
struct ResultLine {
    game_id: u32,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    plies: u32,
    #[serde(default)]
    error: bool,
    #[serde(default)]
    pair_index: Option<u32>,
    #[serde(default)]
    pair_slot: Option<u32>,
    #[serde(default)]
    startpos_idx: Option<u32>,
}

/// 1ペアファイルを索引する。先頭行が `type:"meta"` として解釈できない
/// （JSON として不正・type が違う・`engine_cmd` 等の必須フィールドが無い）場合は
/// 「対局ファイルではない」とみなし、このファイルだけを warning 付きで読み飛ばす
/// （`Ok(None)`、index 全体は中断しない）。一方、`game_id` の行が result 行の後に
/// 再出現する等の**ファイル中盤での**契約違反は `Err` を返し、呼び出し側
/// (`build_index`) の `?` で索引構築全体を中断させる（1ファイルの破損が
/// 他の健全なファイルの可視性を奪わない先頭行と、対局単位の整合性を保証する
/// 必要がある中盤以降とで、扱いを意図的に分けている）。
fn index_one_file(
    path: &Path,
    file_idx: usize,
    entries: &mut Vec<GameIndexEntry>,
    warnings: &mut Vec<String>,
) -> Result<Option<PairFileMeta>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut offset: u64 = 0;
    let mut line_buf = Vec::new();

    let Some(_) = read_line(&mut reader, &mut offset, &mut line_buf)? else {
        return Ok(None); // 空ファイル。対局ファイルとして不自然ではないので warning は出さない。
    };
    let Ok(meta_value) = serde_json::from_slice::<Value>(&line_buf) else {
        warnings.push(format!(
            "{}: 先頭行が JSON として解釈できないため対局ファイルとして扱わず読み飛ばしました",
            path.display()
        ));
        return Ok(None);
    };
    if meta_value.get("type").and_then(Value::as_str) != Some("meta") {
        warnings.push(format!(
            "{}: 先頭行が type:\"meta\" ではないため対局ファイルとして扱わず読み飛ばしました",
            path.display()
        ));
        return Ok(None);
    }
    // meta 行として認識はできたが構造が想定と異なる（`engine_cmd` 欠落等）場合は、
    // 「対局ファイルではない」場合と同様に当該ファイルだけを読み飛ばす。
    // ここで `?` により build_index 全体を中断させると、out-dir 中の1ファイルの
    // schema 不整合で他の健全な数千局が一切見られなくなってしまう。
    let meta_line: MetaLine = match serde_json::from_value(meta_value) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(format!("{}: invalid meta line ({e}), skipped", path.display()));
            return Ok(None);
        }
    };

    let mut open: HashMap<u32, u64> = HashMap::new();
    let mut closed: HashSet<u32> = HashSet::new();
    // 進行中対局 (result 行なし) の手数を索引に載せるため move 行を game_id 別に数える。
    let mut move_counts: HashMap<u32, u32> = HashMap::new();
    // game_id ごとの評価値指標。move 行は interleave しうるので game_id で束ねる。
    let mut evals: HashMap<u32, EvalAccumulator> = HashMap::new();

    // 進行中対局の末尾行は書き込み途中でありうる (csa_client の live 追記を読者が
    // 同時に開くのが正常運用)。末尾の切れた行だけは破損でなく「追記中」とみなして
    // 無視し、その手前までを索引する (次の再読込で完全化する)。
    let mut last_complete_end: u64 = offset;
    while let Some((line_start, line_len)) = read_line(&mut reader, &mut offset, &mut line_buf)? {
        if line_buf.iter().all(|b| b.is_ascii_whitespace()) {
            last_complete_end = line_start + line_len;
            continue;
        }
        let value: Value = match serde_json::from_slice(&line_buf) {
            Ok(v) => v,
            Err(e) => {
                // 改行で終わっていない行 = read_until が EOF に当たった切れた末尾
                // (JSON 行は生の改行を含まないため)。EOF 到達の再確認だと read と
                // 確認の間の追記で誤って破損扱いしうるので、行内容だけで判定する。
                if !line_buf.ends_with(b"\n") {
                    warnings.push(format!(
                        "{}: 末尾行が JSON として不完全のため無視しました (書き込み途中の可能性)",
                        path.display()
                    ));
                    break;
                }
                return Err(e).with_context(|| {
                    format!("{}: invalid JSON line at offset {line_start}", path.display())
                });
            }
        };
        last_complete_end = line_start + line_len;
        match value.get("type").and_then(Value::as_str) {
            Some("move") => {
                let game_id = value.get("game_id").and_then(Value::as_u64).ok_or_else(|| {
                    anyhow!("{}: move line missing game_id at offset {line_start}", path.display())
                })? as u32;
                if closed.contains(&game_id) {
                    bail!(
                        "{}: game_id={game_id} の move 行が result 行の後に再出現しました（offset {line_start}）。\
                         1局分の行は連続しているという前提が崩れているため索引構築を中断します。",
                        path.display()
                    );
                }
                open.entry(game_id).or_insert(line_start);
                *move_counts.entry(game_id).or_default() += 1;
                if let Some(cp) = move_black_pov_cp(&value) {
                    evals.entry(game_id).or_default().push(cp);
                }
            }
            Some("result") => {
                let result: ResultLine = serde_json::from_value(value).with_context(|| {
                    format!("{}: invalid result line at offset {line_start}", path.display())
                })?;
                if closed.contains(&result.game_id) {
                    bail!(
                        "{}: game_id={} の result 行が複数回出現しました（offset {line_start}）",
                        path.display(),
                        result.game_id
                    );
                }
                // move 行が1つも無い対局（エンジン起動失敗等のエラー対局）は
                // result 行自身の offset を開始位置にする。
                let start_offset = open.remove(&result.game_id).unwrap_or(line_start);
                let end_offset = line_start + line_len;
                entries.push(GameIndexEntry {
                    source: GameSourceRef::Jsonl {
                        file_idx,
                        game_id: result.game_id,
                        start_offset,
                        end_offset,
                    },
                    outcome: parse_outcome(&result.outcome),
                    error: result.error,
                    ply_count: result.plies,
                    pair_index: result.pair_index,
                    pair_slot: result.pair_slot,
                    startpos_idx: result.startpos_idx,
                    metrics: evals.remove(&result.game_id).unwrap_or_default().finish(),
                });
                closed.insert(result.game_id);
            }
            Some("meta") => {
                bail!(
                    "{}: meta 行がファイル中盤に再出現しました（offset {line_start}）",
                    path.display()
                );
            }
            _ => {
                // control_history 相当の type 等、対局データを含まない行は無視する。
            }
        }
    }

    if !open.is_empty() {
        // result 行を伴わない対局 = 進行中 (csa_client の live 追記や実行中の tournament)
        // または途中終了。勝敗不明の対局として索引する (live 再読込で完了時に置き換わる)。
        // HashMap の反復順に依存しないよう開始 offset 順で並べる。
        let mut open_games: Vec<(u32, u64)> = open.into_iter().collect();
        open_games.sort_by_key(|&(_, start)| start);
        warnings.push(format!(
            "{}: {} 局が result 行を伴わず終端しました（進行中・途中終了）。勝敗不明として索引します。",
            path.display(),
            open_games.len()
        ));
        for (game_id, start_offset) in open_games {
            entries.push(GameIndexEntry {
                source: GameSourceRef::Jsonl {
                    file_idx,
                    game_id,
                    start_offset,
                    // 切れた末尾行を再生範囲に含めない (完全な行までを読む)。
                    end_offset: last_complete_end,
                },
                outcome: None,
                error: false,
                ply_count: move_counts.get(&game_id).copied().unwrap_or(0),
                pair_index: None,
                pair_slot: None,
                startpos_idx: None,
                metrics: evals.remove(&game_id).unwrap_or_default().finish(),
            });
        }
    }

    Ok(Some(PairFileMeta {
        path: path.to_path_buf(),
        black_label: meta_line.engine_cmd.label_black,
        white_label: meta_line.engine_cmd.label_white,
        date_key: path.file_name().and_then(|n| n.to_str()).and_then(date_key_from_filename),
    }))
}

fn parse_outcome(s: &str) -> Option<GameOutcomeView> {
    match s {
        "black_win" => Some(GameOutcomeView::Win(Color::Black)),
        "white_win" => Some(GameOutcomeView::Win(Color::White)),
        "draw" => Some(GameOutcomeView::Draw),
        _ => None,
    }
}

/// move 行 JSON から評価値を先手視点 cp で取り出す（評価値が無い・範囲外・手番が
/// `"b"`/`"w"` として読めない場合は `None` でスキップ）。`eval.score_cp` は手番相対（USI）
/// なので後手番なら符号を反転する。索引時に sfen を parse せず済ませるため、盤面ではなく
/// `side_to_move` フィールドを使う。
fn move_black_pov_cp(value: &Value) -> Option<i32> {
    let cp = i32::try_from(value.get("eval")?.get("score_cp")?.as_i64()?).ok()?;
    match value.get("side_to_move")?.as_str()? {
        "b" => Some(cp),
        "w" => cp.checked_neg(),
        _ => None,
    }
}

fn build_move_view(line: &MoveLine) -> Result<MoveView> {
    let mut pos = Position::new();
    pos.set_sfen(&line.sfen_before)
        .map_err(|e| anyhow!("failed to parse sfen_before '{}': {e}", line.sfen_before))?;
    let side = pos.side_to_move();
    // JSONL の `ply` は対局内 1 始まりで、定跡途中開始（例: 24手目）の絶対手数を
    // 持たない。SFEN の手数カウンタ（`set_sfen` が `game_ply` に格納）が絶対手数なので
    // そちらを採用する。手数は 1 以上が正常で、0 は不正 SFEN なので debug で検知する。
    let abs_ply = {
        let ply = pos.game_ply();
        debug_assert!(ply >= 1, "SFEN 手数が 0 以下: sfen_before={:?}", line.sfen_before);
        ply.max(1) as u32
    };

    // resign/win/timeout/illegal 等の終局用の擬似指し手は Move::from_usi が None
    // を返すため、実手と区別してそのまま文字列を表示する。
    let (mv, kif_label) = match Move::from_usi(&line.move_usi) {
        Some(mv) => (mv, format_move_label(abs_ply, &pos, mv)),
        None => (Move::NONE, format!("{:>4} {}", abs_ply, line.move_usi)),
    };

    let annotation = MoveAnnotation {
        score_cp: line.eval.as_ref().and_then(|e| e.score_cp),
        score_mate: line.eval.as_ref().and_then(|e| e.score_mate),
        depth: line.eval.as_ref().and_then(|e| e.depth),
        seldepth: line.eval.as_ref().and_then(|e| e.seldepth),
        nodes: line.eval.as_ref().and_then(|e| e.nodes),
        nps: line.eval.as_ref().and_then(|e| e.nps),
        elapsed_ms: line.elapsed_ms,
        timed_out: line.timed_out,
    };

    Ok(MoveView {
        ply: abs_ply,
        side,
        sfen_before: line.sfen_before.clone(),
        mv,
        kif_label,
        annotation,
    })
}

/// `\n` 区切りで1行読み、`(行の開始バイトオフセット, 行のバイト長)` を返す。
/// `offset` は呼び出し側で累積する。EOF (0バイト読めた) では `None`。
fn read_line(
    reader: &mut impl BufRead,
    offset: &mut u64,
    buf: &mut Vec<u8>,
) -> io::Result<Option<(u64, u64)>> {
    buf.clear();
    let start = *offset;
    let n = reader.read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(None);
    }
    *offset += n as u64;
    Ok(Some((start, n as u64)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    const STARTPOS: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    fn write_file(dir: &Path, name: &str, lines: &[String]) -> PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).expect("create");
        for line in lines {
            writeln!(f, "{line}").expect("write line");
        }
        path
    }

    fn meta_line(black: &str, white: &str) -> String {
        format!(
            r#"{{"type":"meta","engine_cmd":{{"label_black":"{black}","label_white":"{white}"}}}}"#
        )
    }

    fn move_line(game_id: u32, ply: u32, move_usi: &str) -> String {
        format!(
            r#"{{"type":"move","game_id":{game_id},"ply":{ply},"side_to_move":"b","sfen_before":"{STARTPOS}","move_usi":"{move_usi}","engine":"e","elapsed_ms":10,"think_limit_ms":100,"timed_out":false}}"#
        )
    }

    fn result_line(game_id: u32, outcome: &str, plies: u32, error: bool) -> String {
        format!(
            r#"{{"type":"result","game_id":{game_id},"outcome":"{outcome}","reason":"r","plies":{plies},"error":{error}}}"#
        )
    }

    #[test]
    fn recognizes_indexed_and_legacy_game_filenames() {
        for name in [
            "pair-0-1__display-black-vs-display-white.jsonl",
            "pair-0-1.jsonl",
            "pair-10-23.jsonl",
            "a-vs-b.jsonl",
            "20260707_A_vs_B.jsonl",
        ] {
            assert!(is_pair_jsonl_name(Path::new(name)), "{name}");
        }
        for name in [
            "pair-.jsonl",
            "pair-0-.jsonl",
            "pair--1.jsonl",
            "pair-a-b.jsonl",
            "pair-0-1-2.jsonl",
            "pair-0-1.jsonl.bak",
            "control_history.jsonl",
            "meta.json",
        ] {
            assert!(!is_pair_jsonl_name(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn indexes_and_loads_indexed_and_legacy_cards_together() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "pair-0-1__display-black-vs-display-white.jsonl",
            "pair-0-1.jsonl",
            "legacy-vs-card.jsonl",
            "20260707_A_vs_B.jsonl",
        ] {
            write_file(
                dir.path(),
                name,
                &[
                    meta_line("from-meta-black", "from-meta-white"),
                    move_line(1, 1, "7g7f"),
                    result_line(1, "black_win", 1, false),
                ],
            );
        }
        write_file(dir.path(), "control_history.jsonl", &["not a game".to_string()]);
        let source = JsonlSource::new(dir.path());
        let index = source.build_index().unwrap();
        assert_eq!(index.pair_files.len(), 4);
        assert_eq!(index.entries.len(), 4);
        assert!(index.warnings.is_empty());
        for meta in &index.pair_files {
            assert_eq!(meta.black_label, "from-meta-black");
            assert_eq!(meta.white_label, "from-meta-white");
        }
        for entry in &index.entries {
            assert_eq!(entry.outcome, Some(GameOutcomeView::Win(Color::Black)));
            let game = source.load_game(&index, entry).unwrap();
            assert_eq!(game.moves.len(), 1);
            assert!(game.moves[0].kif_label.contains('▲'));
        }
    }

    #[test]
    fn move_black_pov_cp_requires_valid_side_and_range() {
        use serde_json::json;
        assert_eq!(
            move_black_pov_cp(&json!({"side_to_move": "b", "eval": {"score_cp": 120}})),
            Some(120)
        );
        // 後手番は符号反転。
        assert_eq!(
            move_black_pov_cp(&json!({"side_to_move": "w", "eval": {"score_cp": 80}})),
            Some(-80)
        );
        // 手番欠落・不明値・評価値なしは先手扱いにせず None でスキップ。
        assert_eq!(move_black_pov_cp(&json!({"eval": {"score_cp": 120}})), None);
        assert_eq!(
            move_black_pov_cp(&json!({"side_to_move": "x", "eval": {"score_cp": 120}})),
            None
        );
        assert_eq!(move_black_pov_cp(&json!({"side_to_move": "b"})), None);
    }

    #[test]
    fn build_move_view_uses_absolute_ply_from_sfen_not_jsonl_ply() {
        // 定跡途中開始（SFEN 手数=24）で、JSONL の対局内 ply=1 ではなく SFEN の
        // 絶対手数 24 を採用することを固定する。
        let json = r#"{"type":"move","game_id":1,"ply":1,"side_to_move":"b","sfen_before":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 24","move_usi":"7g7f","engine":"e","elapsed_ms":10,"think_limit_ms":100,"timed_out":false}"#;
        let line: MoveLine = serde_json::from_str(json).expect("parse move line");
        let view = build_move_view(&line).expect("build move view");
        assert_eq!(view.ply, 24, "SFEN 手数カウンタを絶対手数として使う");
        assert!(view.kif_label.contains("24"), "ラベルにも絶対手数を出す: {}", view.kif_label);
    }

    #[test]
    fn indexes_two_games_in_one_pair_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            dir.path(),
            "a-vs-b.jsonl",
            &[
                meta_line("a", "b"),
                move_line(1, 1, "7g7f"),
                result_line(1, "black_win", 1, false),
                move_line(2, 1, "2g2f"),
                result_line(2, "draw", 1, false),
            ],
        );

        let source = JsonlSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        assert_eq!(index.pair_files.len(), 1);
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].outcome, Some(GameOutcomeView::Win(Color::Black)));
        assert_eq!(index.entries[1].outcome, Some(GameOutcomeView::Draw));

        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves.len(), 1);
        assert!(game.moves[0].kif_label.contains('▲'));
    }

    #[test]
    fn excludes_control_history_and_non_matching_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            dir.path(),
            "control_history.jsonl",
            &[r#"{"type":"control","target_games":10}"#.to_string()],
        );
        write_file(dir.path(), "a-vs-b.jsonl", &[meta_line("a", "b")]);
        // meta.json はそもそも *.jsonl ではないため対象外（拡張子で除外される）。
        std::fs::write(dir.path().join("meta.json"), "{}").expect("write meta.json");

        let index = JsonlSource::new(dir.path()).build_index().expect("build_index");
        assert_eq!(index.pair_files.len(), 1);
        assert_eq!(index.pair_files[0].black_label, "a");
    }

    #[test]
    fn tolerates_torn_tail_line_of_live_file() {
        // live 追記中のファイルは末尾行が書き込み途中でありうる。切れた末尾行は
        // 無視して手前までを索引し、再生範囲にも含めない。
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a-vs-b.jsonl");
        let mut body = format!(
            "{}
{}
",
            meta_line("a", "b"),
            move_line(1, 1, "7g7f")
        );
        body.push_str(r#"{"type":"move","game_id":1,"ply":2,"sfen_"#); // 途中で切れた行
        std::fs::write(&path, body).expect("write");

        let source = JsonlSource::new(dir.path());
        let index = source.build_index().expect("切れた末尾行で index 全体を落とさない");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].ply_count, 1, "完全な行の 1 手だけを数える");
        assert!(index.warnings.iter().any(|w| w.contains("不完全")));
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves.len(), 1, "切れた行は再生範囲に含めない");
    }

    #[test]
    fn accepts_csa_client_per_game_jsonl_names() {
        // csa_client の per-game 記録(`_vs_`)も tournament ペアファイルと同じスキーマで
        // 索引でき、ファイル名から日時キーが取れる。
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "20260707_010203_A_vs_B.jsonl", &[meta_line("A", "B")]);

        let index = JsonlSource::new(dir.path()).build_index().expect("build_index");
        assert_eq!(index.pair_files.len(), 1);
        assert_eq!(index.pair_files[0].black_label, "A");
        assert_eq!(index.pair_files[0].date_key, Some(20260707010203));
    }

    #[test]
    fn tolerates_zero_game_pair_file_and_error_game() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "empty-vs-pair.jsonl", &[meta_line("e1", "e2")]);
        write_file(
            dir.path(),
            "x-vs-y.jsonl",
            &[
                meta_line("x", "y"),
                result_line(1, "draw", 0, true), // move 行 0 件のエラー対局
            ],
        );

        let index = JsonlSource::new(dir.path()).build_index().expect("build_index");
        assert_eq!(index.pair_files.len(), 2);
        assert_eq!(index.entries.len(), 1);
        assert!(index.entries[0].error);
        assert_eq!(index.entries[0].ply_count, 0);
    }

    #[test]
    fn game_id_is_scoped_per_file_not_global() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            dir.path(),
            "a-vs-b.jsonl",
            &[
                meta_line("a", "b"),
                move_line(1, 1, "7g7f"),
                result_line(1, "draw", 1, false),
            ],
        );
        write_file(
            dir.path(),
            "c-vs-d.jsonl",
            &[
                meta_line("c", "d"),
                move_line(1, 1, "2g2f"),
                result_line(1, "draw", 1, false),
            ],
        );

        let index = JsonlSource::new(dir.path()).build_index().expect("build_index");
        assert_eq!(index.entries.len(), 2);
        let GameSourceRef::Jsonl {
            file_idx: f0,
            game_id: g0,
            ..
        } = index.entries[0].source
        else {
            panic!("expected Jsonl source");
        };
        let GameSourceRef::Jsonl {
            file_idx: f1,
            game_id: g1,
            ..
        } = index.entries[1].source
        else {
            panic!("expected Jsonl source");
        };
        assert_eq!((g0, g1), (1, 1), "game_id はペアローカルなので両方とも 1");
        assert_ne!(f0, f1, "file_idx で区別できる");
    }

    #[test]
    fn aborts_on_move_line_after_game_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            dir.path(),
            "a-vs-b.jsonl",
            &[
                meta_line("a", "b"),
                move_line(1, 1, "7g7f"),
                result_line(1, "draw", 1, false),
                move_line(1, 2, "2g2f"), // 既に閉じた game_id=1 の move 行が再出現
            ],
        );

        let err = JsonlSource::new(dir.path()).build_index().expect_err("must abort");
        assert!(format!("{err}").contains("再出現"), "err: {err}");
    }

    #[test]
    fn indexes_incomplete_trailing_game_as_in_progress() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            dir.path(),
            "a-vs-b.jsonl",
            &[
                meta_line("a", "b"),
                move_line(1, 1, "7g7f"),
                move_line(1, 2, "3c3d"), // result 行が無いまま終端 = 進行中
            ],
        );

        let source = JsonlSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].outcome, None);
        assert!(!index.entries[0].error);
        assert_eq!(index.entries[0].ply_count, 2);
        assert!(!index.warnings.is_empty());
        // 進行中対局も既存のバイト範囲経路でそのまま再生できる。
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves.len(), 2);
    }

    #[test]
    fn skips_file_with_malformed_meta_line_without_aborting_whole_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        // type:"meta" だが engine_cmd が欠落している壊れたファイル。
        write_file(dir.path(), "broken-vs-meta.jsonl", &[r#"{"type":"meta"}"#.to_string()]);
        // 健全なファイルも同じ out-dir に置く。
        write_file(
            dir.path(),
            "a-vs-b.jsonl",
            &[
                meta_line("a", "b"),
                move_line(1, 1, "7g7f"),
                result_line(1, "draw", 1, false),
            ],
        );

        let index = JsonlSource::new(dir.path()).build_index().expect("build_index must not abort");
        assert_eq!(index.pair_files.len(), 1, "壊れたファイルは除外され、健全な方だけ残る");
        assert_eq!(index.entries.len(), 1);
        assert!(index.warnings.iter().any(|w| w.contains("broken-vs-meta.jsonl")));
    }

    #[test]
    fn load_game_rejects_byte_range_with_foreign_game_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            dir.path(),
            "a-vs-b.jsonl",
            &[
                meta_line("a", "b"),
                move_line(1, 1, "7g7f"),
                result_line(1, "draw", 1, false),
                move_line(2, 1, "2g2f"),
                result_line(2, "draw", 1, false),
            ],
        );
        let source = JsonlSource::new(dir.path());
        let index = source.build_index().expect("build_index");

        // game_id=2 の実体を指すバイト範囲はそのままに、entry の game_id だけ
        // 1 に書き換えて「索引が壊れている」状態を人為的に再現する。
        let mut tampered = index.entries[1].clone();
        let GameSourceRef::Jsonl {
            file_idx,
            start_offset,
            end_offset,
            ..
        } = tampered.source
        else {
            panic!("expected Jsonl source");
        };
        tampered.source = GameSourceRef::Jsonl {
            file_idx,
            game_id: 1,
            start_offset,
            end_offset,
        };

        let err = source.load_game(&index, &tampered).expect_err("must reject mismatched game_id");
        assert!(format!("{err}").contains("混入"), "err: {err}");
    }
}
