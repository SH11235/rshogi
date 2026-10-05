//! 入玉評価テストセットの構築・採点ツール。
//!
//! CSA replay と勝敗導出は `replay::csa_source::CsaSource` に委譲する。宣言判定は
//! `Position::declaration_win(EnteringKingRule::Point27)` を使う。core が
//! `entering_king_point_info` を公開していないため、点数系フィールドは JSONL に出さない。

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::mem::size_of;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use rshogi_core::nnue::{
    AccumulatorStackVariant, LayerStackBucketMode, LayerStacksAccCache,
    SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, configure_layer_stack_routing, evaluate_dispatch,
    get_network, init_nnue, layer_stack_progress_coeff_required,
    set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, EnteringKingRule, Move, Rank};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::common::io::partial_path;
use crate::packed_sfen::{pack_position_hcp, stm_result_to_hcpe};
use crate::replay::csa_source::CsaSource;
use crate::replay::model::{GameIndex, GameIndexEntry, GameOutcomeView, GameSource, MoveView};
use crate::teacher_labeler::HCPE_RECORD_SIZE;

const DEFAULT_MIN_PLY_FROM_ENTRY: i32 = -20;
const DEFAULT_SAMPLE_STRIDE: u32 = 4;
// cp→勝率の Ponanza 定数 (p = sigmoid(cp/600))。学習側の scale (FV_SCALE 調整用) とは無関係。
const DEFAULT_SCALE: f64 = 600.0;
const DECISIVE_CP: i32 = 600;
const PROB_EPS: f64 = 1e-12;

#[derive(Parser, Debug)]
#[command(
    name = "ek_testset",
    version,
    about = "入玉評価テストセットを CSA から構築し、NNUE 静的評価または hcpe export → yardstick で採点する"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// held-out CSA から入玉評価テストセットを構築する。
    Build(BuildArgs),
    /// testset.jsonl を native NNUE 評価で採点する。
    Eval(EvalArgs),
    /// testset.jsonl を yardstick_label 用 hcpe へ変換する。
    ExportHcpe(ExportHcpeArgs),
}

#[derive(Parser, Debug)]
struct BuildArgs {
    /// 入力 CSA ファイルまたは CSA ディレクトリ。
    #[arg(long)]
    input: PathBuf,
    /// 出力ディレクトリ。
    #[arg(long)]
    out_dir: PathBuf,
    /// 玉の敵陣初侵入 ply から何手前以降を対象にするか。
    #[arg(long, default_value_t = DEFAULT_MIN_PLY_FROM_ENTRY, allow_hyphen_values = true)]
    min_ply_from_entry: i32,
    /// 対象区間を何手ごとにサンプルするか。
    #[arg(long, default_value_t = DEFAULT_SAMPLE_STRIDE)]
    sample_stride: u32,
    /// draw 対局を除外するか。既定 false: 入玉の変換失敗（千日手・持将棋）は
    /// このテストセットが測りたい弱点そのものなので、draw も採点対象に含める。
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    drop_draw: bool,
}

#[derive(Parser, Debug)]
struct EvalArgs {
    /// `ek_testset build` が出した testset.jsonl。
    #[arg(long)]
    testset: PathBuf,
    /// NNUE ファイル。
    #[arg(long)]
    eval_file: PathBuf,
    /// LayerStacks progresskpabs 用 progress.bin。
    /// `--progress-buckets 1` (no-op routing) 以外では必須。
    #[arg(long)]
    progress_file: Option<PathBuf>,
    /// progresskpabs が推論に使う bucket 数。
    #[arg(long)]
    progress_buckets: usize,
    /// sigmoid(eval / scale) の scale。
    #[arg(long, default_value_t = DEFAULT_SCALE)]
    scale: f64,
    /// metrics.json の出力先。
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct ExportHcpeArgs {
    /// `ek_testset build` が出した testset.jsonl。
    #[arg(long)]
    testset: PathBuf,
    /// hcpe（38B/レコード）の出力先。
    #[arg(long)]
    out: PathBuf,
    /// draw レコードを出力から除外するか。
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    drop_draw: bool,
    /// `floodgate_eval_cp` 欠損レコードをエラーにせず出力から除外して続行するか。
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    allow_missing_eval: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct TestsetRecord {
    sfen: String,
    stm: char,
    ply: u32,
    source_csa: String,
    is_declarable: bool,
    dt_label: Option<Label>,
    oc_label: Label,
    #[serde(skip_serializing_if = "Option::is_none")]
    floodgate_eval_cp: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Label {
    Win,
    Loss,
    Draw,
}

#[derive(Debug, Serialize)]
struct BuildMeta {
    input: String,
    min_ply_from_entry: i32,
    sample_stride: u32,
    drop_draw: bool,
    games_indexed: usize,
    games_used: usize,
    games_skipped_broken: usize,
    records: usize,
    dt_records: usize,
    // oc_records は勝敗ラベル（符号一致率の分母）、draw_records は draw（期待スコア 0.5 で採点）。
    oc_records: usize,
    draw_records: usize,
    // 生成元 CSA の一覧は持たない（レコードごとの source_csa が生成元を記録する）。
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EvalMetrics {
    testset: String,
    eval_file: String,
    progress_file: Option<String>,
    progress_buckets: usize,
    scale: f64,
    records: usize,
    dt: DtMetrics,
    oc: OcMetrics,
}

#[derive(Debug, Serialize, Default, PartialEq)]
struct DtMetrics {
    n: usize,
    sign_acc: Option<f64>,
    decisive_acc: Option<f64>,
    eval_median: Option<i32>,
    eval_p10: Option<i32>,
}

#[derive(Debug, Serialize, Default, PartialEq)]
struct OcMetrics {
    n: usize,
    // n のうち draw（期待スコア 0.5 で採点）。sign_acc の分母は n - n_draw。
    n_draw: usize,
    sign_acc: Option<f64>,
    wdl_cross_entropy: Option<f64>,
    brier: Option<f64>,
    calibration: Vec<CalibrationBin>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct CalibrationBin {
    bin: usize,
    n: usize,
    avg_pred: f64,
    // 平均実スコア（win=1 / draw=0.5 / loss=0）。
    score_rate: f64,
}

#[derive(Debug, Clone, Copy)]
struct ScoredLabel {
    eval_cp: i32,
    label: Label,
}

/// OC calibration の固定幅ビン数（予測勝率 [0,1] を等幅分割）。
///
/// 全予測を保持して分位分割する代わりに、ビンごとの件数・予測和・勝敗和だけを
/// 逐次加算する。ピークメモリを入力件数に非依存（ビン数固定）にするため。
const OC_CALIBRATION_BINS: usize = 10;

/// DT 評価値ヒストグラムの片側範囲 [cp]。
///
/// 全評価値を保持する代わりに 1cp 幅の固定長ヒストグラムへ逐次加算し、ピークメモリを
/// 入力件数に非依存にする。範囲は静的評価の実用域を十分覆い、範囲外は端に飽和させる
/// （分位値は範囲内なら正確、範囲外は ±DT_EVAL_HIST_MAX_CP に丸まる）。
const DT_EVAL_HIST_MAX_CP: i32 = 32_000;
const DT_EVAL_HIST_LEN: usize = DT_EVAL_HIST_MAX_CP as usize * 2 + 1;

#[derive(Debug)]
struct EvalMetricBuilder {
    records: usize,
    // index = clamp(eval_cp) + DT_EVAL_HIST_MAX_CP の件数。
    dt_hist: Box<[u64]>,
    oc_n: usize,
    oc_draw: usize,
    oc_sign_ok: usize,
    oc_ce: f64,
    oc_brier: f64,
    // 等幅ビンごとの逐次集計（件数 / 予測勝率和 / 勝敗和）。
    oc_cal_count: [usize; OC_CALIBRATION_BINS],
    oc_cal_pred_sum: [f64; OC_CALIBRATION_BINS],
    oc_cal_win_sum: [f64; OC_CALIBRATION_BINS],
}

impl Default for EvalMetricBuilder {
    fn default() -> Self {
        Self {
            records: 0,
            dt_hist: vec![0; DT_EVAL_HIST_LEN].into_boxed_slice(),
            oc_n: 0,
            oc_draw: 0,
            oc_sign_ok: 0,
            oc_ce: 0.0,
            oc_brier: 0.0,
            oc_cal_count: [0; OC_CALIBRATION_BINS],
            oc_cal_pred_sum: [0.0; OC_CALIBRATION_BINS],
            oc_cal_win_sum: [0.0; OC_CALIBRATION_BINS],
        }
    }
}

#[derive(Debug, Clone)]
struct PositionSample {
    sfen: String,
    ply: u32,
    floodgate_eval_cp: Option<i32>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ExportHcpeStats {
    output_records: usize,
    draw_records: usize,
    eval_clamped: usize,
    eval_missing: usize,
}

/// CLI entrypoint。
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build(args) => run_build(&args),
        Command::Eval(args) => run_eval(&args),
        Command::ExportHcpe(args) => run_export_hcpe(&args),
    }
}

fn run_build(args: &BuildArgs) -> Result<()> {
    if args.sample_stride == 0 {
        bail!("--sample-stride は 1 以上を指定してください");
    }

    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("出力ディレクトリを作成できません: {}", args.out_dir.display()))?;

    let testset_path = args.out_dir.join("testset.jsonl");
    let sfens_path = args.out_dir.join("sfens.txt");
    let meta_path = args.out_dir.join("meta.json");
    let mut testset = BufWriter::new(File::create(&testset_path)?);
    let mut sfens = BufWriter::new(File::create(&sfens_path)?);

    let mut records = 0usize;
    let mut dt_records = 0usize;
    let mut oc_records = 0usize;
    let mut draw_records = 0usize;
    let mut games_indexed = 0usize;
    let mut games_used = 0usize;
    let mut games_skipped_broken = 0usize;

    // 対局ごとの index/メタやパス一覧をコーパス全体分保持しないよう、CSA を
    // 決定的な順序の遅延走査で 1 ファイル = 1 対局ずつ読み込んで処理する。
    for path in csa_paths(&args.input)? {
        let path = path?;
        let source = CsaSource::new(&path);
        let index = source.build_index()?;
        for warning in &index.warnings {
            eprintln!("warning: {warning}");
        }
        games_indexed += index.entries.len();

        for entry in &index.entries {
            let Some(outcome) = entry.outcome else {
                continue;
            };
            if args.drop_draw && matches!(outcome, GameOutcomeView::Draw) {
                continue;
            }

            let game = source.load_game(&index, entry)?;
            let source_csa = source_path(&index, entry)?;
            if !replay_is_complete(&game.moves, entry.ply_count) {
                games_skipped_broken += 1;
                eprintln!(
                    "warning: {source_csa}: 再生を末尾まで信頼できないため対局ごと除外します"
                );
                continue;
            }
            let built = build_records_for_game(
                &game.moves,
                outcome,
                &source_csa,
                args.min_ply_from_entry,
                args.sample_stride,
            )?;
            if !built.is_empty() {
                games_used += 1;
            }
            for record in built {
                if record.dt_label == Some(Label::Win) {
                    dt_records += 1;
                }
                match record.oc_label {
                    Label::Win | Label::Loss => oc_records += 1,
                    Label::Draw => draw_records += 1,
                }
                serde_json::to_writer(&mut testset, &record)?;
                writeln!(testset)?;
                writeln!(sfens, "{}", record.sfen)?;
                records += 1;
            }
        }
    }
    testset.flush()?;
    sfens.flush()?;

    let meta = BuildMeta {
        input: args.input.display().to_string(),
        min_ply_from_entry: args.min_ply_from_entry,
        sample_stride: args.sample_stride,
        drop_draw: args.drop_draw,
        games_indexed,
        games_used,
        games_skipped_broken,
        records,
        dt_records,
        oc_records,
        draw_records,
        notes: vec![
            "core が entering_king_point_info を公開していないため points_stm / king_in_enemy_stm / enemy_zone_pieces_stm は省略".to_string(),
        ],
    };
    let mut meta_writer = BufWriter::new(File::create(&meta_path)?);
    serde_json::to_writer_pretty(&mut meta_writer, &meta)?;
    writeln!(meta_writer)?;
    meta_writer.flush()?;

    eprintln!(
        "wrote {} records (dt={}, oc={}, draw={}) to {}",
        records,
        dt_records,
        oc_records,
        draw_records,
        testset_path.display()
    );
    Ok(())
}

/// 入力がディレクトリなら配下の `*.csa` を、単一ファイルならそれ 1 つを列挙する。
///
/// 全パスを収集・保持せず、ディレクトリごとにファイル名ソートした DFS で遅延走査する
/// （ピークメモリはディレクトリの fan-out に比例し、総ファイル数に非依存）。
/// `follow_links(false)` で symlink は辿らない（ループ回避）。走査エラーは握りつぶさず
/// `Err` として返す（欠落したサブツリーを「完全な held-out セット」と誤認させないため）。
fn csa_paths(input: &Path) -> Result<Box<dyn Iterator<Item = Result<PathBuf>>>> {
    let md = fs::metadata(input)
        .with_context(|| format!("入力を確認できません: {}", input.display()))?;
    if md.is_dir() {
        Ok(Box::new(
            WalkDir::new(input)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_map(|entry| match entry {
                    Ok(e) => (e.file_type().is_file()
                        && e.path().extension().and_then(|x| x.to_str()) == Some("csa"))
                    .then(|| Ok(e.into_path())),
                    Err(e) => Some(Err(anyhow!(e).context("入力ディレクトリの走査に失敗しました"))),
                }),
        ))
    } else {
        Ok(Box::new(std::iter::once(Ok(input.to_path_buf()))))
    }
}

fn build_records_for_game(
    moves: &[MoveView],
    outcome: GameOutcomeView,
    source_csa: &str,
    min_ply_from_entry: i32,
    sample_stride: u32,
) -> Result<Vec<TestsetRecord>> {
    let samples = position_samples(moves)?;
    let first_entry_ply = first_enemy_entry_ply(&samples)?;
    let Some(first_entry_ply) = first_entry_ply else {
        return Ok(Vec::new());
    };
    let threshold = i64::from(first_entry_ply) + i64::from(min_ply_from_entry);
    let mut records = Vec::new();

    for sample in samples {
        let ply = i64::from(sample.ply);
        if ply < threshold {
            continue;
        }
        if (ply - threshold).rem_euclid(i64::from(sample_stride)) != 0 {
            continue;
        }

        let mut pos = Position::new();
        pos.set_sfen(&sample.sfen)
            .with_context(|| format!("SFEN を復元できません: {}", sample.sfen))?;
        let stm = pos.side_to_move();
        let is_declarable = pos.declaration_win(EnteringKingRule::Point27) != Move::NONE;
        let oc_label = oc_label_for_stm(outcome, stm);

        records.push(TestsetRecord {
            sfen: sample.sfen,
            stm: color_label(stm),
            ply: sample.ply,
            source_csa: source_csa.to_string(),
            is_declarable,
            dt_label: is_declarable.then_some(Label::Win),
            oc_label,
            floodgate_eval_cp: sample.floodgate_eval_cp,
        });
    }

    Ok(records)
}

fn position_samples(moves: &[MoveView]) -> Result<Vec<PositionSample>> {
    let mut samples = Vec::with_capacity(moves.len() + 1);
    for mv in moves {
        samples.push(PositionSample {
            sfen: mv.sfen_before.clone(),
            ply: mv.ply,
            floodgate_eval_cp: mv.annotation.score_cp,
        });
    }

    if let Some(last) = moves.last()
        && last.mv.is_normal()
    {
        let mut pos = Position::new();
        pos.set_sfen(&last.sfen_before)
            .with_context(|| format!("SFEN を復元できません: {}", last.sfen_before))?;
        let gives_check = pos.gives_check(last.mv);
        pos.do_move(last.mv, gives_check);
        samples.push(PositionSample {
            sfen: pos.to_sfen(),
            ply: last.ply.saturating_add(1),
            floodgate_eval_cp: None,
        });
    }

    Ok(samples)
}

fn first_enemy_entry_ply(samples: &[PositionSample]) -> Result<Option<u32>> {
    let mut first = None;
    for sample in samples {
        let mut pos = Position::new();
        pos.set_sfen(&sample.sfen)
            .with_context(|| format!("SFEN を復元できません: {}", sample.sfen))?;
        if king_in_enemy_zone(&pos, Color::Black) || king_in_enemy_zone(&pos, Color::White) {
            first = Some(sample.ply);
            break;
        }
    }
    Ok(first)
}

fn king_in_enemy_zone(pos: &Position, side: Color) -> bool {
    match side {
        Color::Black => {
            matches!(pos.king_square(side).rank(), Rank::Rank1 | Rank::Rank2 | Rank::Rank3)
        }
        Color::White => {
            matches!(pos.king_square(side).rank(), Rank::Rank7 | Rank::Rank8 | Rank::Rank9)
        }
    }
}

/// 再生が末尾まで信頼できるか。
///
/// `CsaSource::load_game` は core 上で信頼できない手を `Move::NONE` にして再生を打ち切る。
/// 打ち切られた対局の途中局面へ最終結果ラベルを付けると誤ラベルになるため、対局ごと除外する。
fn replay_is_complete(moves: &[MoveView], expected_normal_moves: u32) -> bool {
    moves.len() as u64 == u64::from(expected_normal_moves) && moves.iter().all(|m| m.mv.is_normal())
}

fn oc_label_for_stm(outcome: GameOutcomeView, stm: Color) -> Label {
    match outcome {
        GameOutcomeView::Win(winner) if winner == stm => Label::Win,
        GameOutcomeView::Win(_) => Label::Loss,
        GameOutcomeView::Draw => Label::Draw,
    }
}

fn source_path(index: &GameIndex, entry: &GameIndexEntry) -> Result<String> {
    let crate::replay::model::GameSourceRef::Csa { file_idx, .. } = entry.source else {
        bail!("CSA 以外の GameIndexEntry が渡されました");
    };
    let meta = index
        .pair_file(file_idx)
        .ok_or_else(|| anyhow!("file_idx {file_idx} が index にありません"))?;
    Ok(meta.path.display().to_string())
}

fn color_label(c: Color) -> char {
    match c {
        Color::Black => 'b',
        Color::White => 'w',
    }
}

fn run_export_hcpe(args: &ExportHcpeArgs) -> Result<()> {
    // 中断時の途中書きが正常な hcpe サイズ (38B の倍数) で残らないよう、`.partial` へ
    // 書いて成功時のみ最終パスへ rename する (hcpe_to_psv と同じ方式)。
    let tmp_output = partial_path(&args.out);

    // 入力と出力 (一時ファイル含む) が同一実体だと File::create が読み取り前に入力を
    // truncate してしまうため拒否する。判定は crate 共通の ensure_safe_output_path に
    // 委ねる (hardlink 検出・symlink 出力拒否・NotFound 以外の比較エラーを握り潰さない)。
    crate::output_path::ensure_safe_output_path(&args.out, &args.testset)?;
    crate::output_path::ensure_safe_output_path(&tmp_output, &args.testset)?;

    let input = File::open(&args.testset)
        .with_context(|| format!("testset を開けません: {}", args.testset.display()))?;
    // 前回中断の残骸 `.partial` が既存 `<out>` 等への hardlink だと File::create が
    // 追跡 truncate でリンク先を壊すため、entry を消してから新規作成する
    // (「正常完了時のみ最終パスへ反映」の保証を staging 経路でも守る)。
    // symlink の `.partial` は ensure_safe_output_path が先に拒否している。
    if let Err(err) = fs::remove_file(&tmp_output)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(err).with_context(|| {
            format!("既存の一時ファイル {} を削除できません", tmp_output.display())
        });
    }
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_output)
        .with_context(|| format!("hcpe を出力できません: {}", tmp_output.display()))?;
    let result = (|| -> Result<ExportHcpeStats> {
        let mut writer = BufWriter::new(output);
        let stats = export_hcpe(
            BufReader::new(input),
            &mut writer,
            &args.testset,
            args.drop_draw,
            args.allow_missing_eval,
        )?;
        // rename 直後の電源断で最終パスにゼロ埋め/途中までの実体が残らないよう、
        // rename 前に fsync する。Windows は開いたままの rename に失敗し得るため、
        // sync 後にここで閉じる。IntoInnerError は開いた writer を内包するため、
        // into_error() で writer ごと破棄してから伝播する (開いたままだと後段の
        // `.partial` 削除が Windows で失敗する)。
        writer.into_inner().map_err(|err| err.into_error())?.sync_all()?;
        fs::rename(&tmp_output, &args.out).with_context(|| {
            format!("{} → {} の rename に失敗", tmp_output.display(), args.out.display())
        })?;
        Ok(stats)
    })();
    let stats = match result {
        Ok(stats) => stats,
        Err(err) => {
            // 途中失敗の `.partial` は 38B の倍数の妥当なサイズになり得るため、
            // 偏った部分集合が誤って採点に使われないよう残さず消す。
            // 削除失敗は元エラーを主に残しつつ警告で報告する。
            if let Err(remove_err) = fs::remove_file(&tmp_output)
                && remove_err.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!(
                    "warning: 一時ファイル {} を削除できません: {remove_err}",
                    tmp_output.display()
                );
            }
            return Err(err);
        }
    };

    eprintln!("{}", export_hcpe_summary(&stats, &args.out));
    Ok(())
}

fn export_hcpe_summary(stats: &ExportHcpeStats, output_path: &Path) -> String {
    let mut summary = format!(
        "wrote {} records (draw={}, eval_clamped={}, eval_missing_skipped={}) to {}",
        stats.output_records,
        stats.draw_records,
        stats.eval_clamped,
        stats.eval_missing,
        output_path.display(),
    );
    if stats.eval_missing > 0 {
        summary.push_str(&format!(
            "\nfloodgate_eval_cp 欠損 {} 件は出力から除外しました (yardstick は保存 eval から \
             eval_band / mate_ref を作って採点に使うため 0 埋めしない)。",
            stats.eval_missing
        ));
    }
    summary
}

fn export_hcpe<R: BufRead, W: Write>(
    reader: R,
    writer: &mut W,
    input_path: &Path,
    drop_draw: bool,
    allow_missing_eval: bool,
) -> Result<ExportHcpeStats> {
    let mut stats = ExportHcpeStats::default();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line
            .with_context(|| format!("{}:{}: 行を読めません", input_path.display(), line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let record: TestsetRecord = serde_json::from_str(&line).with_context(|| {
            format!("{}:{}: JSON を読めません", input_path.display(), line_no + 1)
        })?;

        // 検証 (stm/SFEN 整合・盤面 pack) は除外判定より先に行い、--drop-draw や
        // --allow-missing-eval が整合検査を迂回しないようにする。
        let (bytes, eval_clamped, eval_missing) =
            encode_hcpe_record(&record).with_context(|| {
                format!("{}:{}: hcpe レコードへ変換できません", input_path.display(), line_no + 1)
            })?;

        // clamp は教師値スケールの汚染検知が目的なので、--drop-draw で出力から
        // 除外されるレコードの分も入力側で計上する。
        stats.eval_clamped += usize::from(eval_clamped);

        if eval_missing {
            if !allow_missing_eval {
                bail!(
                    "{}:{}: floodgate_eval_cp がありません。yardstick_label は保存 eval から \
                     eval_band / mate_ref を作り、yardstick_score が mate 除外・a_ref 較正・\
                     参照系指標に使うため 0 埋めできません。該当レコードを除外して続行するには \
                     --allow-missing-eval true を指定してください",
                    input_path.display(),
                    line_no + 1
                );
            }
            stats.eval_missing += 1;
            if record.oc_label == Label::Draw {
                stats.draw_records += 1;
            }
            continue;
        }
        if record.oc_label == Label::Draw {
            stats.draw_records += 1;
            if drop_draw {
                continue;
            }
        }

        writer.write_all(&bytes)?;
        stats.output_records += 1;
    }
    Ok(stats)
}

fn encode_hcpe_record(record: &TestsetRecord) -> Result<([u8; HCPE_RECORD_SIZE], bool, bool)> {
    let stm = match record.stm {
        'b' => Color::Black,
        'w' => Color::White,
        other => bail!("stm は b/w のいずれかである必要があります: {other}"),
    };

    let mut pos = Position::new();
    pos.set_sfen(&record.sfen)
        .with_context(|| format!("SFEN を読めません: {}", record.sfen))?;
    if pos.side_to_move() != stm {
        bail!(
            "stm ({}) と SFEN の手番 ({}) が一致しません",
            record.stm,
            color_label(pos.side_to_move())
        );
    }

    let eval_missing = record.floodgate_eval_cp.is_none();
    let eval_cp = record.floodgate_eval_cp.unwrap_or(0);
    let clamped_eval = eval_cp.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
    let eval_clamped = clamped_eval != eval_cp;
    let stored_eval = i16::try_from(clamped_eval).expect("i16 範囲へ clamp 済み");
    let stm_result = match record.oc_label {
        Label::Win => 1,
        Label::Loss => -1,
        Label::Draw => 0,
    };

    let mut bytes = [0u8; HCPE_RECORD_SIZE];
    bytes[0..32].copy_from_slice(&pack_position_hcp(&pos));
    bytes[32..34].copy_from_slice(&stored_eval.to_le_bytes());
    // bestMove16 は「指し手なし」の 0。padding も初期値の 0 のままにする。
    bytes[36] = stm_result_to_hcpe(stm_result, stm);
    Ok((bytes, eval_clamped, eval_missing))
}

fn run_eval(args: &EvalArgs) -> Result<()> {
    if !args.scale.is_finite() || args.scale <= 0.0 {
        bail!("--scale は正の有限値を指定してください");
    }

    if let Some(progress_file) = &args.progress_file {
        let weights = load_progress_coeff_kpabs(progress_file)
            .map_err(|e| anyhow!("progress 読み込みに失敗しました: {e}"))?;
        set_layer_stack_progress_kpabs_weights(weights)
            .map_err(|e| anyhow!("progress 設定に失敗しました: {e}"))?;
    } else if layer_stack_progress_coeff_required(Some(args.progress_buckets)) {
        bail!("--progress-buckets が 2 以上のときは --progress-file が必須です");
    }
    init_nnue(&args.eval_file)
        .with_context(|| format!("NNUE を読み込めません: {}", args.eval_file.display()))?;

    let network = get_network().ok_or_else(|| anyhow!("NNUE が初期化されていません"))?;
    if !network.is_layer_stacks() {
        bail!(
            "ek_testset eval は LayerStacks NNUE のみ対応しています: {}",
            network.architecture_name()
        );
    }
    configure_layer_stack_routing(
        LayerStackBucketMode::ProgressKPAbs,
        network.layer_stack_num_buckets().expect("LayerStacks checked above"),
        Some(args.progress_buckets),
    )
    .map_err(anyhow::Error::msg)?;
    let mut stack = AccumulatorStackVariant::from_network(&network);
    // acc_cache (Finny Tables) は静的 LayerStacks variant 専用の API で、
    // runtime-dimensions ビルドでは同じ net でも DynamicLayerStacks として load され
    // `as_layer_stacks()` が panic する (`is_layer_stacks()` は Dynamic でも true)。
    // `evaluate_dispatch` は None でも全 variant を正しく評価するため cache は使わない。
    let mut acc_cache: Option<LayerStacksAccCache> = None;

    let mut builder = EvalMetricBuilder::default();
    let file = File::open(&args.testset)
        .with_context(|| format!("testset を開けません: {}", args.testset.display()))?;
    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: TestsetRecord = serde_json::from_str(&line).with_context(|| {
            format!("{}:{}: JSON を読めません", args.testset.display(), line_no + 1)
        })?;
        let mut pos = Position::new();
        pos.set_sfen(&record.sfen).with_context(|| {
            format!("{}:{}: SFEN を読めません", args.testset.display(), line_no + 1)
        })?;
        stack.reset();
        // 内部 Value スケール (歩=90) ではなく cp (歩=100) で採点する。DECISIVE_CP /
        // --scale / floodgate_eval_cp と単位を揃えるため to_cp() で換算する。
        let eval_cp = evaluate_dispatch(&pos, &mut stack, &mut acc_cache).to_cp();
        builder.push(&record, eval_cp, args.scale);
    }

    let (records, dt, oc) = builder.finish();
    let out = EvalMetrics {
        testset: args.testset.display().to_string(),
        eval_file: args.eval_file.display().to_string(),
        progress_file: args.progress_file.as_ref().map(|p| p.display().to_string()),
        progress_buckets: args.progress_buckets,
        scale: args.scale,
        records,
        dt,
        oc,
    };

    let stdout = std::io::stdout();
    let mut locked = stdout.lock();
    serde_json::to_writer_pretty(&mut locked, &out)?;
    writeln!(locked)?;

    if let Some(path) = &args.out {
        write_json_pretty(path, &out)?;
    }
    Ok(())
}

fn load_progress_coeff_kpabs(path: &Path) -> Result<Box<[f32]>, String> {
    let bytes = fs::read(path).map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
    let expected = SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * size_of::<f64>();
    if bytes.len() != expected {
        return Err(format!(
            "progress.bin size mismatch: got {} bytes, expected {}",
            bytes.len(),
            expected
        ));
    }
    let weights: Vec<f32> = bytes
        .as_chunks::<{ size_of::<f64>() }>()
        .0
        .iter()
        .map(|chunk| f64::from_le_bytes(*chunk) as f32)
        .collect();
    Ok(weights.into_boxed_slice())
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut writer = BufWriter::new(
        File::create(path).with_context(|| format!("出力できません: {}", path.display()))?,
    );
    serde_json::to_writer_pretty(&mut writer, value)?;
    writeln!(writer)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
fn compute_metrics(records: &[(TestsetRecord, i32)], scale: f64) -> (DtMetrics, OcMetrics) {
    let mut builder = EvalMetricBuilder::default();
    for (record, eval_cp) in records {
        builder.push(record, *eval_cp, scale);
    }
    let (_, dt, oc) = builder.finish();
    (dt, oc)
}

impl EvalMetricBuilder {
    fn push(&mut self, record: &TestsetRecord, eval_cp: i32, scale: f64) {
        self.records += 1;
        if record.dt_label == Some(Label::Win) {
            let clamped = eval_cp.clamp(-DT_EVAL_HIST_MAX_CP, DT_EVAL_HIST_MAX_CP);
            self.dt_hist[(clamped + DT_EVAL_HIST_MAX_CP) as usize] += 1;
        }
        self.push_oc(
            ScoredLabel {
                eval_cp,
                label: record.oc_label,
            },
            scale,
        );
    }

    fn push_oc(&mut self, record: ScoredLabel, scale: f64) {
        // draw は入玉の変換失敗（千日手・持将棋）の署名そのものなので採点対象に含め、
        // 期待スコア 0.5 として CE/Brier/calibration に算入する。符号一致率は勝敗のみ。
        let target = match record.label {
            Label::Win => 1.0,
            Label::Draw => 0.5,
            Label::Loss => 0.0,
        };
        match record.label {
            Label::Win if record.eval_cp > 0 => self.oc_sign_ok += 1,
            Label::Loss if record.eval_cp < 0 => self.oc_sign_ok += 1,
            Label::Draw => self.oc_draw += 1,
            _ => {}
        }
        let p = sigmoid(f64::from(record.eval_cp) / scale).clamp(PROB_EPS, 1.0 - PROB_EPS);
        self.oc_n += 1;
        self.oc_ce += -(target * p.ln() + (1.0 - target) * (1.0 - p).ln());
        self.oc_brier += (p - target).powi(2);
        // 予測勝率 p を等幅ビンへ振り分けて逐次加算（[0,1) を BINS 等分、p=1.0 は最終ビン）。
        let bin = ((p * OC_CALIBRATION_BINS as f64) as usize).min(OC_CALIBRATION_BINS - 1);
        self.oc_cal_count[bin] += 1;
        self.oc_cal_pred_sum[bin] += p;
        self.oc_cal_win_sum[bin] += target;
    }

    fn finish(self) -> (usize, DtMetrics, OcMetrics) {
        let oc = if self.oc_n == 0 {
            OcMetrics::default()
        } else {
            let decisive = self.oc_n - self.oc_draw;
            OcMetrics {
                n: self.oc_n,
                n_draw: self.oc_draw,
                sign_acc: (decisive > 0).then(|| rate(self.oc_sign_ok, decisive)),
                wdl_cross_entropy: Some(self.oc_ce / self.oc_n as f64),
                brier: Some(self.oc_brier / self.oc_n as f64),
                calibration: self.calibration_bins(),
            }
        };
        (self.records, dt_metrics(&self.dt_hist), oc)
    }

    /// 等幅ビンの逐次集計から calibration テーブルを構築する（空ビンは省く）。
    fn calibration_bins(&self) -> Vec<CalibrationBin> {
        let mut out = Vec::new();
        for bin in 0..OC_CALIBRATION_BINS {
            let n = self.oc_cal_count[bin];
            if n == 0 {
                continue;
            }
            out.push(CalibrationBin {
                bin,
                n,
                avg_pred: self.oc_cal_pred_sum[bin] / n as f64,
                score_rate: self.oc_cal_win_sum[bin] / n as f64,
            });
        }
        out
    }
}

fn dt_metrics(hist: &[u64]) -> DtMetrics {
    let n: u64 = hist.iter().sum();
    if n == 0 {
        return DtMetrics::default();
    }
    // cp > threshold の件数（threshold は DT_EVAL_HIST_MAX_CP 未満が前提）。
    let count_above = |threshold: i32| -> u64 {
        hist[(threshold + 1 + DT_EVAL_HIST_MAX_CP) as usize..].iter().sum()
    };
    DtMetrics {
        n: n as usize,
        sign_acc: Some(count_above(0) as f64 / n as f64),
        decisive_acc: Some(count_above(DECISIVE_CP) as f64 / n as f64),
        eval_median: Some(hist_percentile(hist, n, 0.5)),
        eval_p10: Some(hist_percentile(hist, n, 0.1)),
    }
}

/// ソート列の `floor((n-1)*q)` 番目に相当する値をヒストグラムの累積和で求める。
fn hist_percentile(hist: &[u64], n: u64, q: f64) -> i32 {
    debug_assert!(n > 0);
    let target = ((n - 1) as f64 * q).floor() as u64;
    let mut cum = 0u64;
    for (idx, &count) in hist.iter().enumerate() {
        cum += count;
        if cum > target {
            return idx as i32 - DT_EVAL_HIST_MAX_CP;
        }
    }
    unreachable!("target < n のため累積和は必ず target を超える")
}

fn rate(num: usize, den: usize) -> f64 {
    num as f64 / den as f64
}

fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packed_sfen::{pack_position, unpack_hcp};

    const STARTPOS_BOARD: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL";

    fn export_record(stm: char, oc_label: Label, eval_cp: i32) -> TestsetRecord {
        TestsetRecord {
            sfen: format!("{STARTPOS_BOARD} {stm} - 1"),
            stm,
            ply: 1,
            source_csa: "game.csa".to_string(),
            is_declarable: false,
            dt_label: None,
            oc_label,
            floodgate_eval_cp: Some(eval_cp),
        }
    }

    fn records_jsonl(records: &[TestsetRecord]) -> Vec<u8> {
        let mut input = Vec::new();
        for record in records {
            serde_json::to_writer(&mut input, record).expect("serialize record");
            input.push(b'\n');
        }
        input
    }

    fn write_csa(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).expect("create");
        f.write_all(text.as_bytes()).expect("write");
        path
    }

    fn build_records_from_csa(text: &str) -> Vec<TestsetRecord> {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "game.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let entry = &index.entries[0];
        let outcome = entry.outcome.expect("outcome");
        let game = source.load_game(&index, entry).expect("load_game");
        build_records_for_game(&game.moves, outcome, "game.csa", -20, 1).expect("records")
    }

    const DECLARABLE_CSA: &str = concat!(
        "V2.2\n",
        "P+51OU11HI21KA31KI41KI61GI71GI81KE91KE12KY22KY00HI00FU00FU00FU00FU00FU00FU00FU00FU00FU\n",
        "P-59OU00FU\n",
        "+\n",
        "'* 1200\n",
        "+0044FU\n",
        "'* -30\n",
        "-0056FU\n",
        "%KACHI\n",
    );

    const ENTERED_RESIGN_CSA: &str =
        concat!("V2.2\n", "P+51OU00FU\n", "P-59OU\n", "+\n", "'* -80\n", "+0044FU\n", "%TORYO\n",);

    #[test]
    fn drop_draw_flag_is_settable() {
        let base = ["ek_testset", "build", "--input", "in", "--out-dir", "out"];
        let cli = Cli::try_parse_from(base).expect("parse default");
        let Command::Build(args) = cli.command else {
            panic!("build expected")
        };
        assert!(!args.drop_draw);

        let with_true = base.iter().chain(&["--drop-draw", "true"]);
        let cli = Cli::try_parse_from(with_true).expect("parse --drop-draw true");
        let Command::Build(args) = cli.command else {
            panic!("build expected")
        };
        assert!(args.drop_draw);
    }

    #[test]
    fn export_hcpe_drop_draw_flag_is_settable() {
        let base = [
            "ek_testset",
            "export-hcpe",
            "--testset",
            "in.jsonl",
            "--out",
            "out.hcpe",
        ];
        let cli = Cli::try_parse_from(base).expect("parse default");
        let Command::ExportHcpe(args) = cli.command else {
            panic!("export-hcpe expected")
        };
        assert!(!args.drop_draw);

        let with_true = base.iter().chain(&["--drop-draw", "true"]);
        let cli = Cli::try_parse_from(with_true).expect("parse --drop-draw true");
        let Command::ExportHcpe(args) = cli.command else {
            panic!("export-hcpe expected")
        };
        assert!(args.drop_draw);
        assert!(!args.allow_missing_eval);

        let with_allow = base.iter().chain(&["--allow-missing-eval", "true"]);
        let cli = Cli::try_parse_from(with_allow).expect("parse --allow-missing-eval true");
        let Command::ExportHcpe(args) = cli.command else {
            panic!("export-hcpe expected")
        };
        assert!(args.allow_missing_eval);
    }

    #[test]
    fn export_hcpe_round_trips_position_with_existing_hcp_reader() {
        let record = export_record('w', Label::Win, 123);
        let (bytes, eval_clamped, eval_missing) = encode_hcpe_record(&record).expect("encode hcpe");
        assert!(!eval_clamped);
        assert!(!eval_missing);

        let mut hcp = [0u8; 32];
        hcp.copy_from_slice(&bytes[0..32]);
        let decoded_sfen = unpack_hcp(&hcp).expect("unpack hcp");
        let mut decoded = Position::new();
        decoded.set_sfen(&decoded_sfen).expect("set decoded SFEN");
        let mut original = Position::new();
        original.set_sfen(&record.sfen).expect("set original SFEN");

        assert_eq!(pack_position(&decoded), pack_position(&original));
        assert_eq!(bytes[34..36], [0, 0]);
        assert_eq!(bytes[37], 0);
    }

    #[test]
    fn export_hcpe_converts_stm_results_to_absolute_results() {
        for (stm, label, expected) in [
            ('b', Label::Win, 1),
            ('b', Label::Loss, 2),
            ('w', Label::Win, 2),
            ('w', Label::Loss, 1),
        ] {
            let record = export_record(stm, label, 0);
            let (bytes, _, _) = encode_hcpe_record(&record).expect("encode hcpe");
            assert_eq!(bytes[36], expected, "stm={stm} label={label:?}");
        }
    }

    #[test]
    fn export_hcpe_keeps_or_drops_draws() {
        let records = [
            export_record('b', Label::Win, 10),
            export_record('w', Label::Draw, -20),
        ];
        let input = records_jsonl(&records);

        let mut kept = Vec::new();
        let kept_stats = export_hcpe(
            std::io::Cursor::new(&input),
            &mut kept,
            Path::new("testset.jsonl"),
            false,
            false,
        )
        .expect("export with draws");
        assert_eq!(
            kept_stats,
            ExportHcpeStats {
                output_records: 2,
                draw_records: 1,
                eval_clamped: 0,
                eval_missing: 0,
            }
        );
        assert_eq!(kept.len(), 2 * HCPE_RECORD_SIZE);
        assert_eq!(kept[HCPE_RECORD_SIZE + 36], 0);

        let mut dropped = Vec::new();
        let dropped_stats = export_hcpe(
            std::io::Cursor::new(&input),
            &mut dropped,
            Path::new("testset.jsonl"),
            true,
            false,
        )
        .expect("export without draws");
        assert_eq!(
            dropped_stats,
            ExportHcpeStats {
                output_records: 1,
                draw_records: 1,
                eval_clamped: 0,
                eval_missing: 0,
            }
        );
        assert_eq!(dropped.len(), HCPE_RECORD_SIZE);
    }

    #[test]
    fn export_hcpe_clamps_eval_to_i16_boundaries() {
        for (input, expected, was_clamped) in [
            (i32::from(i16::MIN) - 1, i16::MIN, true),
            (i32::from(i16::MIN), i16::MIN, false),
            (i32::from(i16::MAX), i16::MAX, false),
            (i32::from(i16::MAX) + 1, i16::MAX, true),
        ] {
            let record = export_record('b', Label::Win, input);
            let (bytes, actual_clamped, eval_missing) =
                encode_hcpe_record(&record).expect("encode hcpe");
            assert_eq!(i16::from_le_bytes([bytes[32], bytes[33]]), expected);
            assert_eq!(actual_clamped, was_clamped);
            assert!(!eval_missing);
        }
    }

    fn missing_eval_jsonl() -> Vec<u8> {
        // TestsetRecord は skip_serializing_if 付きなので、明示 `null` ケースは
        // serde_json::Value 経由で作る (struct を serialize するとフィールド欠落になる)。
        let mut null_eval = serde_json::to_value(export_record('b', Label::Win, 1))
            .expect("serialize null-eval record");
        null_eval["floodgate_eval_cp"] = serde_json::Value::Null;
        let mut missing_eval = serde_json::to_value(export_record('w', Label::Loss, 2))
            .expect("serialize missing-eval record");
        missing_eval
            .as_object_mut()
            .expect("record is an object")
            .remove("floodgate_eval_cp");
        let following = export_record('b', Label::Draw, 345);

        let mut input = Vec::new();
        serde_json::to_writer(&mut input, &null_eval).expect("serialize null-eval record");
        input.push(b'\n');
        serde_json::to_writer(&mut input, &missing_eval).expect("serialize missing-eval record");
        input.push(b'\n');
        serde_json::to_writer(&mut input, &following).expect("serialize following record");
        input.push(b'\n');
        input
    }

    #[test]
    fn export_hcpe_missing_eval_is_an_error_by_default() {
        let input = missing_eval_jsonl();
        let mut output = Vec::new();
        let err = export_hcpe(
            std::io::Cursor::new(input),
            &mut output,
            Path::new("testset.jsonl"),
            false,
            false,
        )
        .expect_err("missing eval must error by default");
        let message = format!("{err:#}");
        assert!(message.contains("testset.jsonl:1"), "unexpected error: {message}");
        assert!(message.contains("--allow-missing-eval"), "unexpected error: {message}");
    }

    #[test]
    fn export_hcpe_skips_missing_evals_when_allowed() {
        let input = missing_eval_jsonl();
        let mut output = Vec::new();
        let stats = export_hcpe(
            std::io::Cursor::new(input),
            &mut output,
            Path::new("testset.jsonl"),
            false,
            true,
        )
        .expect("export with --allow-missing-eval");

        assert_eq!(
            stats,
            ExportHcpeStats {
                output_records: 1,
                draw_records: 1,
                eval_clamped: 0,
                eval_missing: 2,
            }
        );
        // 欠損 2 件は出力されず、eval ありの 1 件 (345) だけが残る。
        assert_eq!(output.len(), HCPE_RECORD_SIZE);
        assert_eq!(i16::from_le_bytes([output[32], output[33]]), 345);

        let summary = export_hcpe_summary(&stats, Path::new("out.hcpe"));
        assert!(summary.contains("eval_missing_skipped=2"));
        assert!(summary.contains("欠損 2 件は出力から除外しました"));
    }

    #[test]
    fn export_hcpe_validates_even_when_missing_eval_is_allowed() {
        // --allow-missing-eval true は欠損 eval の除外だけを許可し、stm/SFEN 整合検査は
        // 迂回しない。
        let mut record = export_record('b', Label::Win, 0);
        record.stm = 'w';
        record.floodgate_eval_cp = None;
        let input = records_jsonl(&[record]);

        let mut output = Vec::new();
        let err = export_hcpe(
            std::io::Cursor::new(input),
            &mut output,
            Path::new("testset.jsonl"),
            false,
            true,
        )
        .expect_err("mismatched stm must error even with --allow-missing-eval");
        assert!(format!("{err:#}").contains("一致しません"));
        assert!(output.is_empty());
    }

    #[test]
    fn export_hcpe_counts_overlapping_draw_and_missing_eval_once_each() {
        let mut record = export_record('b', Label::Draw, 0);
        record.floodgate_eval_cp = None;
        let input = records_jsonl(&[record]);

        let mut output = Vec::new();
        let stats = export_hcpe(
            std::io::Cursor::new(input),
            &mut output,
            Path::new("testset.jsonl"),
            false,
            true,
        )
        .expect("export draw+missing record");
        assert_eq!(
            stats,
            ExportHcpeStats {
                output_records: 0,
                draw_records: 1,
                eval_clamped: 0,
                eval_missing: 1,
            }
        );
        assert!(output.is_empty());
    }

    #[test]
    fn export_hcpe_validates_records_before_dropping_draws() {
        // stm と SFEN の手番が矛盾した draw レコードは --drop-draw true でもエラーにする
        // (除外パスが整合検査を迂回しない)。
        let mut record = export_record('b', Label::Draw, 0);
        record.stm = 'w';
        let input = records_jsonl(&[record]);

        let mut output = Vec::new();
        let err = export_hcpe(
            std::io::Cursor::new(input),
            &mut output,
            Path::new("testset.jsonl"),
            true,
            false,
        )
        .expect_err("mismatched stm must error even for dropped draws");
        assert!(format!("{err:#}").contains("一致しません"));
        assert!(output.is_empty());
    }

    #[test]
    fn run_export_hcpe_rejects_same_input_and_output_and_stages_partial() {
        let dir = tempfile::tempdir().expect("tempdir");
        let testset = dir.path().join("testset.jsonl");
        fs::write(&testset, records_jsonl(&[export_record('b', Label::Win, 10)]))
            .expect("write testset");

        // 入力と出力が同一実体なら truncate 前に拒否し、入力を壊さない。
        let err = run_export_hcpe(&ExportHcpeArgs {
            testset: testset.clone(),
            out: testset.clone(),
            drop_draw: false,
            allow_missing_eval: false,
        })
        .expect_err("same input/output must be rejected");
        assert!(format!("{err:#}").contains("resolves to input file"));
        assert!(fs::metadata(&testset).expect("input must survive").len() > 0);

        // 正常系は .partial 経由で最終パスへ rename され、.partial は残らない。
        let out = dir.path().join("out.hcpe");
        run_export_hcpe(&ExportHcpeArgs {
            testset: testset.clone(),
            out: out.clone(),
            drop_draw: false,
            allow_missing_eval: false,
        })
        .expect("export to a fresh output");
        assert_eq!(fs::metadata(&out).expect("output exists").len(), HCPE_RECORD_SIZE as u64);
        assert!(!partial_path(&out).exists());
    }

    #[test]
    fn run_export_hcpe_rejects_hardlinks_to_input() {
        let dir = tempfile::tempdir().expect("tempdir");
        let testset = dir.path().join("testset.jsonl");
        let content = records_jsonl(&[export_record('b', Label::Win, 10)]);
        fs::write(&testset, &content).expect("write testset");

        // 出力が入力への hardlink (別パス・同一実体)。
        let out_link = dir.path().join("out-link.hcpe");
        fs::hard_link(&testset, &out_link).expect("hardlink out");
        let err = run_export_hcpe(&ExportHcpeArgs {
            testset: testset.clone(),
            out: out_link,
            drop_draw: false,
            allow_missing_eval: false,
        })
        .expect_err("hardlinked output must be rejected");
        assert!(format!("{err:#}").contains("resolves to input file"));

        // `.partial` が入力への hardlink。
        let out = dir.path().join("out.hcpe");
        fs::hard_link(&testset, partial_path(&out)).expect("hardlink partial");
        let err = run_export_hcpe(&ExportHcpeArgs {
            testset: testset.clone(),
            out,
            drop_draw: false,
            allow_missing_eval: false,
        })
        .expect_err("hardlinked .partial must be rejected");
        assert!(format!("{err:#}").contains("resolves to input file"));

        // どちらの経路でも入力は無傷。
        assert_eq!(fs::read(&testset).expect("read input back"), content);
    }

    #[test]
    fn run_export_hcpe_failure_keeps_existing_output_intact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let testset = dir.path().join("testset.jsonl");
        // 1 行目は変換に成功し、2 行目の欠損 eval で失敗する入力。
        let ok = export_record('b', Label::Win, 10);
        let mut broken = export_record('w', Label::Loss, 0);
        broken.floodgate_eval_cp = None;
        fs::write(&testset, records_jsonl(&[ok, broken])).expect("write testset");

        // 既存の最終出力と、それへの hardlink として残った前回の `.partial`。
        let out = dir.path().join("out.hcpe");
        fs::write(&out, b"sentinel").expect("write existing output");
        fs::hard_link(&out, partial_path(&out)).expect("stale partial hardlink");

        run_export_hcpe(&ExportHcpeArgs {
            testset,
            out: out.clone(),
            drop_draw: false,
            allow_missing_eval: false,
        })
        .expect_err("missing eval must fail the export");

        // 途中失敗では既存出力を truncate も置換もせず、書きかけの `.partial` も残さない。
        assert_eq!(fs::read(&out).expect("existing output survives"), b"sentinel");
        assert!(!partial_path(&out).exists());
    }

    #[test]
    fn build_labels_declarable_csa() {
        let records = build_records_from_csa(DECLARABLE_CSA);
        assert!(!records.is_empty());
        let terminal = records.iter().find(|r| r.ply == 3).expect("terminal sample");
        assert_eq!(terminal.stm, 'b');
        assert!(terminal.is_declarable);
        assert_eq!(terminal.dt_label, Some(Label::Win));
        assert_eq!(terminal.oc_label, Label::Win);
        assert_eq!(terminal.floodgate_eval_cp, None);
    }

    #[test]
    fn build_keeps_side_relative_floodgate_eval_for_white_stm() {
        let records = build_records_from_csa(DECLARABLE_CSA);
        let white = records.iter().find(|r| r.stm == 'w').expect("white sample");
        assert_eq!(white.floodgate_eval_cp, Some(30));
    }

    #[test]
    fn build_labels_entered_but_not_declarable_resign_csa() {
        let records = build_records_from_csa(ENTERED_RESIGN_CSA);
        assert!(!records.is_empty());
        let r = &records[0];
        assert_eq!(r.stm, 'b');
        assert!(!r.is_declarable);
        assert_eq!(r.dt_label, None);
        assert_eq!(r.oc_label, Label::Win);
        assert_eq!(r.floodgate_eval_cp, Some(-80));
    }

    // 打歩は 1 段目に打てないため、core の合法手ゲートで `Move::NONE` にフォールバックする。
    const ILLEGAL_MOVE_CSA: &str =
        concat!("V2.2\n", "P+51OU00FU\n", "P-59OU\n", "+\n", "+0011FU\n", "%TORYO\n",);

    fn load_first_game(text: &str) -> (Vec<MoveView>, u32) {
        let dir = tempfile::tempdir().expect("tempdir");
        write_csa(dir.path(), "game.csa", text);
        let source = CsaSource::new(dir.path());
        let index = source.build_index().expect("build_index");
        let entry = &index.entries[0];
        let game = source.load_game(&index, entry).expect("load_game");
        (game.moves, entry.ply_count)
    }

    #[test]
    fn replay_is_complete_accepts_trusted_and_rejects_illegal_replay() {
        let (moves, ply_count) = load_first_game(DECLARABLE_CSA);
        assert!(replay_is_complete(&moves, ply_count));

        let (moves, ply_count) = load_first_game(ILLEGAL_MOVE_CSA);
        assert!(!replay_is_complete(&moves, ply_count));
    }

    #[test]
    fn all_draw_input_yields_no_sign_acc() {
        let draw = TestsetRecord {
            sfen: "4k4/9/9/9/9/9/9/9/4K4 b - 1".to_string(),
            stm: 'b',
            ply: 1,
            source_csa: String::new(),
            is_declarable: false,
            dt_label: None,
            oc_label: Label::Draw,
            floodgate_eval_cp: None,
        };
        let records = vec![(draw.clone(), 100), (draw, -100)];
        let (_, oc) = compute_metrics(&records, 600.0);
        assert_eq!(oc.n, 2);
        assert_eq!(oc.n_draw, 2);
        assert_eq!(oc.sign_acc, None);
        assert!(oc.wdl_cross_entropy.unwrap() > 0.0);
    }

    #[test]
    fn dt_metrics_from_histogram_match_sorted_semantics() {
        let dt = |eval_cp: i32| {
            let r = TestsetRecord {
                sfen: "4k4/9/9/9/9/9/9/9/4K4 b - 1".to_string(),
                stm: 'b',
                ply: 1,
                source_csa: String::new(),
                is_declarable: true,
                dt_label: Some(Label::Win),
                oc_label: Label::Win,
                floodgate_eval_cp: None,
            };
            (r, eval_cp)
        };
        // -40000 は端 (-32000) に飽和する。ソート列は [-32000, 0, 100, 700]。
        let records = vec![dt(700), dt(-40_000), dt(0), dt(100)];
        let (dt, _) = compute_metrics(&records, 100.0);
        assert_eq!(dt.n, 4);
        assert_eq!(dt.sign_acc, Some(0.5));
        assert_eq!(dt.decisive_acc, Some(0.25));
        assert_eq!(dt.eval_median, Some(0));
        assert_eq!(dt.eval_p10, Some(-32_000));
    }

    #[test]
    fn eval_metrics_are_checked_from_synthetic_values() {
        let base = TestsetRecord {
            sfen: "4k4/9/9/9/9/9/9/9/4K4 b - 1".to_string(),
            stm: 'b',
            ply: 1,
            source_csa: "x.csa".to_string(),
            is_declarable: false,
            dt_label: None,
            oc_label: Label::Win,
            floodgate_eval_cp: None,
        };
        let mut dt_win = base.clone();
        dt_win.is_declarable = true;
        dt_win.dt_label = Some(Label::Win);
        let mut loss = base.clone();
        loss.oc_label = Label::Loss;
        let mut draw = base.clone();
        draw.oc_label = Label::Draw;
        let records = vec![(dt_win, 700), (base, 100), (loss, -100), (draw, 0)];
        let (dt, oc) = compute_metrics(&records, 100.0);
        assert_eq!(dt.n, 1);
        assert_eq!(dt.sign_acc, Some(1.0));
        assert_eq!(dt.decisive_acc, Some(1.0));
        assert_eq!(dt.eval_median, Some(700));
        assert_eq!(oc.n, 4);
        assert_eq!(oc.n_draw, 1);
        // 符号一致率の分母は勝敗の 3 件のみ（draw は含めない）。
        assert_eq!(oc.sign_acc, Some(1.0));
        // draw (eval=0) は p=0.5, target=0.5 で CE=ln2, Brier=0 を寄与する。
        let expected_p = sigmoid(1.0);
        let expected_ce =
            (-((expected_p.ln() * 2.0) + sigmoid(7.0).ln()) + std::f64::consts::LN_2) / 4.0;
        assert!((oc.wdl_cross_entropy.unwrap() - expected_ce).abs() < 1e-12);
        assert_eq!(oc.calibration.iter().map(|b| b.n).sum::<usize>(), 4);
    }
}
