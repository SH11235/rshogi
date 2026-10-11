//! CSA 棋譜（rshogi csa_client 出力形式）を対象にした `GameSource` 実装。
//!
//! CSA は 1 ファイル = 1 対局。`--csa` にディレクトリを渡すと配下の `*.csa` を
//! 横断して 1 つの対局リストにフラット化し、単一ファイルを渡すとその 1 局だけを開く。
//!
//! 指し手・盤面の復元は `rshogi_csa::parse_csa_full`（独自の軽量 Position/Move 型）に
//! 委譲し、`sfen_before` 経由で `rshogi_core` 側へ橋渡しして棋譜ラベルを組み立てる。
//! 評価値は floodgate 形式コメントから拾う。2 系統の記録に対応する:
//! - 旧版 rshogi csa_client の自前記録: `'* <score> [pv...]` を**手の直前**に書く
//! - wdoor (shogi-server) の公開棋譜と現行 csa_client の記録: `'** <score> [pv...]` を
//!   **手の直後** (T 行の後) に書く
//!
//! `<score>` はどちらも先手視点 (csa_client は送信時に正規化、floodgate 規約も先手視点)
//! なので、手番相対へ戻して格納する。`T<秒>` 行は直前の手の消費時間として拾う。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};
use rshogi_csa::{
    Color as CsaColor, EvalCommentStyle, ParsedMove, SpecialMove, csa_move_to_usi,
    parse_csa_full_with_evals_style, usi_move_to_csa,
};
use walkdir::WalkDir;

use crate::kif::format_move_label;

use super::model::{
    EvalAccumulator, GameIndex, GameIndexEntry, GameOutcomeView, GameRecord, GameSource,
    GameSourceRef, MoveAnnotation, MoveView, PairFileMeta, date_key_from_filename,
    fingerprint_paths, move_is_legal,
};

/// CSA 棋譜（rshogi csa_client 出力形式）の `GameSource` 実装。
///
/// `--csa <dir|file>` に渡すパスを受け取り、`build_index`/`load_game` を提供する。
/// ディレクトリを渡すと配下の `*.csa` を再帰収集して 1 つの対局リストとして扱う。
pub struct CsaSource {
    /// ディレクトリ（配下の `*.csa` を横断）または単一 `.csa` ファイル。
    input: PathBuf,
    eval_comment_style: EvalCommentStyle,
}

impl CsaSource {
    /// `input` はディレクトリ（配下の `*.csa` を再帰横断）または単一 `.csa` ファイル。
    pub fn new(input: impl Into<PathBuf>) -> Self {
        Self {
            input: input.into(),
            eval_comment_style: EvalCommentStyle::Standard,
        }
    }

    pub fn with_legacy_server_eval_comments(mut self, enabled: bool) -> Self {
        self.eval_comment_style = if enabled {
            EvalCommentStyle::LegacyServerPost
        } else {
            EvalCommentStyle::Standard
        };
        self
    }

    /// 入力がディレクトリなら配下の `*.csa` を（サブディレクトリも再帰して）パス順で、
    /// 単一ファイルならそれ 1 つを返す。floodgate は `YYYY/MM/DD/*.csa` のように日付
    /// ディレクトリへネストするため再帰する。実行のたびに `file_idx` を安定させるよう
    /// 全体を収集してからソートする。`follow_links(false)` で symlink は辿らない（ループ回避）。
    fn collect_paths(&self) -> Result<Vec<PathBuf>> {
        let md = fs::metadata(&self.input)
            .with_context(|| format!("failed to stat {}", self.input.display()))?;
        if md.is_dir() {
            let mut paths: Vec<PathBuf> = WalkDir::new(&self.input)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().is_file()
                        && e.path().extension().and_then(|x| x.to_str()) == Some("csa")
                })
                .map(|e| e.into_path())
                .collect();
            paths.sort();
            Ok(paths)
        } else {
            Ok(vec![self.input.clone()])
        }
    }
}

impl GameSource for CsaSource {
    fn build_index(&self) -> Result<GameIndex> {
        let paths = self.collect_paths()?;

        let mut entries = Vec::new();
        let mut pair_files = Vec::new();
        let mut warnings = Vec::new();

        for path in &paths {
            let text = match fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    warnings.push(format!(
                        "{}: 読み込みに失敗したため読み飛ばしました（{e}）",
                        path.display()
                    ));
                    continue;
                }
            };
            let (init, parsed, info, evals) =
                match parse_csa_full_with_evals_style(&text, self.eval_comment_style) {
                    Ok(v) => v,
                    Err(e) => {
                        warnings.push(format!(
                            "{}: CSA として解釈できないため読み飛ばしました（{e}）",
                            path.display()
                        ));
                        continue;
                    }
                };

            let normal_count =
                parsed.iter().filter(|m| matches!(m, ParsedMove::Normal(_))).count() as u32;
            let outcome = derive_outcome(init.side_to_move, normal_count, &parsed);

            // 評価値コメントは元々先手視点なので、そのまま指標へ流す。
            let mut acc = EvalAccumulator::default();
            for cp in evals.into_iter().flatten() {
                acc.push(cp);
            }

            let file_idx = pair_files.len();
            let ordinal = entries.len() as u32;
            entries.push(GameIndexEntry {
                source: GameSourceRef::Csa { file_idx, ordinal },
                outcome,
                error: false,
                ply_count: normal_count,
                pair_index: None,
                pair_slot: None,
                startpos_idx: None,
                metrics: acc.finish(),
            });
            let date_key =
                path.file_name().and_then(|n| n.to_str()).and_then(date_key_from_filename);
            pair_files.push(PairFileMeta {
                path: path.clone(),
                black_label: info.black_name.unwrap_or_else(|| "先手".to_string()),
                white_label: info.white_name.unwrap_or_else(|| "後手".to_string()),
                date_key,
            });
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
        let GameSourceRef::Csa { file_idx, .. } = entry.source else {
            bail!("CsaSource::load_game received a non-CSA GameIndexEntry");
        };
        let meta = index
            .pair_file(file_idx)
            .ok_or_else(|| anyhow!("file_idx {file_idx} not found in index"))?;
        let text = fs::read_to_string(&meta.path)
            .with_context(|| format!("failed to read {}", meta.path.display()))?;
        let (mut pos, parsed, _info, evals) =
            parse_csa_full_with_evals_style(&text, self.eval_comment_style)
                .with_context(|| format!("failed to parse {}", meta.path.display()))?;
        let mut evals = evals.into_iter();
        let mut moves = Vec::new();
        for pm in &parsed {
            let ParsedMove::Normal(cm) = pm else {
                continue; // 終局特殊手は再生対象の指し手列には含めない。
            };
            let eval_cp_black = evals.next().flatten();
            let sfen_before = pos.to_sfen();

            // ラベル・手番・絶対手数は rshogi_core 側の局面から取る（JSONL/PSV と同一の
            // `format_move_label` を通すため）。SFEN の相互変換に失敗した場合でも 1 手で
            // 対局全体を落とさず、素の CSA 文字列にフォールバックする。
            let mut core = Position::new();
            let core_ok = core.set_sfen(&sfen_before).is_ok();
            let side = if core_ok {
                core.side_to_move()
            } else {
                to_core_color(pos.side_to_move)
            };
            let abs_ply: u32 = if core_ok {
                core.game_ply().max(1) as u32
            } else {
                pos.ply.max(1)
            };

            let usi = csa_move_to_usi(&cm.mv, &pos).ok();
            // csa_move_to_usi は駒種を落とす（成り判定にしか使わない）ため、usi→CSA 逆変換が
            // 元の CSA 手に戻るかで駒種の整合も確かめる。駒種を偽った破損手（例: 歩の位置から
            // の `+7776GI`）は apply_csa_move が誤った駒を盤へ置き以降の局面が壊れるため、
            // 通常手にせず盤面追跡もそこで打ち切る。
            let csa_consistent = usi
                .as_deref()
                .and_then(|u| usi_move_to_csa(u, &pos).ok())
                .is_some_and(|back| back == cm.mv);
            let applied = pos.apply_csa_move(&cm.mv).is_ok();

            // 通常手として `mv` を持たせるのは「駒種まで整合し、sfen_before が core 側で復元でき、
            // その局面の合法手集合に `mv` が含まれる」ときだけ。`render_board` の `do_move`
            // （`promote().unwrap()` 等）も `format_move_label`（空マス発で `piece_type()` panic）も
            // 合法手を前提にしており、`apply_csa_move` は駒種・成りの妥当性まで検証しないため、
            // 合法手生成と CSA 逆変換の一致で確実にゲートする。満たさない手は `Move::NONE` ＋
            // 生 CSA ラベルへフォールバックする。
            let legal_mv = usi
                .as_deref()
                .and_then(Move::from_usi)
                .filter(|&mv| csa_consistent && core_ok && move_is_legal(&core, mv));
            let (mv, kif_label) = match legal_mv {
                Some(mv) => (mv, format_move_label(abs_ply, &core, mv)),
                None => (Move::NONE, format!("{:>4} {}", abs_ply, cm.mv)),
            };

            let score_cp =
                eval_cp_black.and_then(|black_pov| black_pov_to_side_relative(black_pov, side));

            moves.push(MoveView {
                ply: abs_ply,
                side,
                sfen_before,
                mv,
                kif_label,
                annotation: MoveAnnotation {
                    score_cp,
                    // 消費時間は parse_csa_full が独立 T 行・インライン `,T` の両方から
                    // 格納済みの値を使う（%TORYO 等に付く T は Normal でないため混入しない）。
                    elapsed_ms: cm.time_sec.map(|s| u64::from(s) * 1000),
                    ..Default::default()
                },
            });

            // 通常手として信頼できない手（駒種不整合・非合法・適用失敗）が出たら、盤面追跡は
            // これ以上信頼できないので打ち切る。
            if legal_mv.is_none() || !applied {
                break;
            }
        }

        // 終局特殊手の生値。`derive_outcome` と同じ「末尾から最初の Special」走査で拾う
        // （勝敗への縮約はせず、`%KACHI` 等の終局理由をそのまま伝える）。
        let termination = parsed.iter().rev().find_map(|m| match m {
            ParsedMove::Special(sp) => Some(sp.clone()),
            ParsedMove::Normal(_) => None,
        });

        Ok(GameRecord {
            moves,
            leading_gap_is_drop: false,
            termination,
        })
    }
}

fn black_pov_to_side_relative(black_pov: i32, side: Color) -> Option<i32> {
    // 先手視点 → 手番相対（後手手番は符号反転）。グラフ側の `black_pov_cp` が
    // 再度 手番相対 → 先手視点 に戻すので、全ソースで格納形式を揃える。
    match side {
        Color::Black => Some(black_pov),
        Color::White => black_pov.checked_neg(),
    }
}

fn to_core_color(c: CsaColor) -> Color {
    match c {
        CsaColor::Black => Color::Black,
        CsaColor::White => Color::White,
    }
}

fn opposite(c: CsaColor) -> CsaColor {
    match c {
        CsaColor::Black => CsaColor::White,
        CsaColor::White => CsaColor::Black,
    }
}

/// 終端の特殊手と、そこでの手番から勝敗を導出する。CSA の `%TORYO` は勝ち側・負け側
/// どちらの記録にも書かれるため、勝者は「終端で手番だった側（＝投了・時間切れ・反則の
/// 当事者）が負け」という規約から求める。終端の特殊手が無い（中断・未完）ファイルは
/// `None`（勝敗不明）。
fn derive_outcome(
    initial_side: CsaColor,
    normal_count: u32,
    parsed: &[ParsedMove],
) -> Option<GameOutcomeView> {
    let sp = parsed.iter().rev().find_map(|m| match m {
        ParsedMove::Special(sp) => Some(sp),
        ParsedMove::Normal(_) => None,
    })?;
    // 通常手を normal_count 手指した後の手番＝終端で指す番だった側。
    let terminal_side = if normal_count.is_multiple_of(2) {
        initial_side
    } else {
        opposite(initial_side)
    };
    match sp {
        SpecialMove::Resign | SpecialMove::TimeUp | SpecialMove::IllegalMove => {
            Some(GameOutcomeView::Win(to_core_color(opposite(terminal_side))))
        }
        SpecialMove::Win => Some(GameOutcomeView::Win(to_core_color(terminal_side))),
        SpecialMove::Draw
        | SpecialMove::Sennichite
        | SpecialMove::Jishogi
        | SpecialMove::MaxMoves => Some(GameOutcomeView::Draw),
        // 中断は結果なし。
        SpecialMove::Interrupt => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::Path;

    fn write_csa(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).expect("create");
        f.write_all(text.as_bytes()).expect("write");
        path
    }

    const RESIGN_GAME: &str =
        "V2.2\nN+SenteEng\nN-GoteEng\nPI\n'* 45\n+7776FU\nT10\n-3334FU\nT12\n%TORYO\n";

    #[test]
    fn indexes_resign_game_with_players_and_winner() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", RESIGN_GAME);
        let index = CsaSource::new(dir.path()).build_index().expect("build_index");

        assert_eq!(index.pair_files.len(), 1);
        assert_eq!(index.pair_files[0].black_label, "SenteEng");
        assert_eq!(index.pair_files[0].white_label, "GoteEng");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].ply_count, 2);
        // 2 手指した後（Black 手番）に %TORYO ＝ Black 投了 ＝ White 勝ち。
        assert_eq!(index.entries[0].outcome, Some(GameOutcomeView::Win(Color::White)));
    }

    #[test]
    fn loads_moves_labels_and_side_relative_score() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", RESIGN_GAME);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");

        assert_eq!(game.moves.len(), 2);
        assert_eq!(game.moves[0].side, Color::Black);
        assert!(game.moves[0].kif_label.contains('▲'), "先手手は ▲: {}", game.moves[0].kif_label);
        // 先手手の先手視点 +45 はそのまま手番相対 +45。
        assert_eq!(game.moves[0].annotation.score_cp, Some(45));
        // 後手手はコメント無しなので評価値なし。
        assert_eq!(game.moves[1].side, Color::White);
        assert_eq!(game.moves[1].annotation.score_cp, None);
    }

    #[test]
    fn converts_black_pov_score_to_side_relative_for_white_move() {
        // 後手手に先手視点 -30 のコメント → 手番相対（後手視点）は +30。
        let text = "V2.2\nN+S\nN-G\nPI\n+7776FU\nT1\n'* -30\n-3334FU\nT1\n%TORYO\n";
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves[1].side, Color::White);
        assert_eq!(game.moves[1].annotation.score_cp, Some(30));
    }

    #[test]
    fn parses_wdoor_style_comments_after_move_and_times() {
        // wdoor (shogi-server) 記録: 手 → T秒 → '** コメント の順。コメントは直前の手に
        // 帰属し、T は消費時間として拾う。%TORYO 後の T は最終手の時間を上書きしない。
        let text = "V2\nN+A\nN-B\nPI\n+7776FU\nT2\n'** 54 -3334FU +2726FU\n-3334FU\nT0\n'** -10\n%TORYO\nT9\n";
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");

        assert_eq!(game.moves.len(), 2);
        // 先手手: 先手視点 +54 → 手番相対 +54、T2 → 2000ms。
        assert_eq!(game.moves[0].annotation.score_cp, Some(54));
        assert_eq!(game.moves[0].annotation.elapsed_ms, Some(2000));
        // 後手手: 先手視点 -10 → 手番相対 +10、T0 → 0ms (%TORYO 後の T9 に上書きされない)。
        assert_eq!(game.moves[1].annotation.score_cp, Some(10));
        assert_eq!(game.moves[1].annotation.elapsed_ms, Some(0));
        // 索引側の評価値指標にも両方流れる (グラフ「表示できる評価値がありません」の回帰防止)。
        assert!(index.entries[0].metrics.final_cp.is_some());
    }

    #[test]
    fn parses_own_record_times_from_t_lines() {
        // csa_client 自前記録: '* コメント → 手 → T秒 の順。T は直前の手に帰属する。
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", RESIGN_GAME);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves[0].annotation.elapsed_ms, Some(10_000));
        assert_eq!(game.moves[1].annotation.elapsed_ms, Some(12_000));
    }

    #[test]
    fn parses_inline_time_suffix_on_move_line() {
        // CSA 標準のインライン形式 `+7776FU,T3` からも消費時間を拾う
        // (parse_csa_full が time_sec に格納する経路)。
        let text = "V2.2\nN+S\nN-G\nPI\n+7776FU,T3\n-3334FU,T4\n%TORYO\n";
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves[0].annotation.elapsed_ms, Some(3_000));
        assert_eq!(game.moves[1].annotation.elapsed_ms, Some(4_000));
    }

    #[test]
    fn legacy_server_eval_style_is_available_to_consumer() {
        let text = "V2.2\n$GAME_ID:legacy\nPI\n+7776FU,T1\n'* 100\n-3334FU,T1\n%TORYO\n";
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", text);
        let source = CsaSource::new(dir.path()).with_legacy_server_eval_comments(true);
        let index = source.build_index().expect("build_index");
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves[0].annotation.score_cp, Some(100));
        assert_eq!(game.moves[1].annotation.score_cp, None);
    }

    #[test]
    fn minimum_i32_white_eval_does_not_overflow() {
        assert_eq!(black_pov_to_side_relative(i32::MIN, Color::White), None);
        assert_eq!(black_pov_to_side_relative(i32::MIN, Color::Black), Some(i32::MIN));
    }

    #[test]
    fn kachi_winner_is_side_to_move() {
        // 1 手指した後（White 手番）に %KACHI ＝ White の入玉宣言勝ち。
        let text = "V2.2\nN+S\nN-G\nPI\n+7776FU\nT1\n%KACHI\n";
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", text);
        let index = CsaSource::new(dir.path()).build_index().expect("build_index");
        assert_eq!(index.entries[0].outcome, Some(GameOutcomeView::Win(Color::White)));
    }

    #[test]
    fn termination_keeps_raw_special_move() {
        // %KACHI は Win、%TORYO は Resign、終局手なしは None として生値を保持する。
        let cases: [(&str, Option<SpecialMove>); 3] = [
            ("V2.2\nN+S\nN-G\nPI\n+7776FU\nT1\n%KACHI\n", Some(SpecialMove::Win)),
            (RESIGN_GAME, Some(SpecialMove::Resign)),
            ("V2.2\nN+S\nN-G\nPI\n+7776FU\nT1\n", None),
        ];
        for (text, expected) in cases {
            let dir = tempfile::tempdir().expect("tempdir");
            write_csa(dir.path(), "a.csa", text);
            let source = CsaSource::new(dir.path());
            let index = source.build_index().expect("build_index");
            let game = source.load_game(&index, &index.entries[0]).expect("load_game");
            assert_eq!(game.termination, expected);
        }
    }

    #[test]
    fn truncated_game_has_unknown_outcome() {
        // 終局手が無い（中断・未完）ファイルは勝敗不明。
        let text = "V2.2\nN+S\nN-G\nPI\n+7776FU\nT1\n-3334FU\nT1\n";
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", text);
        let index = CsaSource::new(dir.path()).build_index().expect("build_index");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].outcome, None);
    }

    #[test]
    fn illegal_move_falls_back_to_none_and_truncates() {
        // 合法な初手のあと、駒の無いマスからの不正手（apply_csa_move が Err を返す）。
        // その手は通常手にせず Move::NONE へフォールバックし、以降を打ち切る
        // （render_board が通常手を合法前提で do_move するため）。
        let text = "V2.2\nN+S\nN-G\nPI\n+7776FU\nT1\n-5556FU\nT1\n%TORYO\n";
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves.len(), 2);
        assert!(game.moves[0].mv.is_normal(), "初手は通常手");
        assert!(!game.moves[1].mv.is_normal(), "不正手は Move::NONE にフォールバック");
    }

    #[test]
    fn piece_type_mismatch_falls_back_to_none_and_truncates() {
        // 初期局面の 77 は歩だが、駒種を偽って `+7776GI`（銀）と書いた破損手。usi 変換は駒種を
        // 落として 7g7f になるが、逆変換で `+7776FU` に戻り不一致 → 通常手にせず打ち切る。
        let text = "V2.2\nN+S\nN-G\nPI\n+7776GI\nT1\n%TORYO\n";
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "a.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let game = source.load_game(&index, &index.entries[0]).expect("load_game");
        assert_eq!(game.moves.len(), 1);
        assert!(!game.moves[0].mv.is_normal(), "駒種を偽った手は Move::NONE にフォールバック");
    }

    #[test]
    fn single_file_input_opens_one_game() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_csa(dir.path(), "solo.csa", RESIGN_GAME);
        let index = CsaSource::new(&path).build_index().expect("build_index");
        assert_eq!(index.entries.len(), 1);
    }

    #[test]
    fn dir_enumerates_csa_in_sorted_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "b.csa", RESIGN_GAME);
        write_csa(dir.path(), "a.csa", RESIGN_GAME);
        // .csa 以外は無視。
        write_csa(dir.path(), "note.txt", "not a game");
        let index = CsaSource::new(dir.path()).build_index().expect("build_index");
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.pair_files[0].path.file_name().unwrap(), "a.csa");
        assert_eq!(index.pair_files[1].path.file_name().unwrap(), "b.csa");
    }
}
